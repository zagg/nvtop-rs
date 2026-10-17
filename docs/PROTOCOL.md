# The protocol, in one page

What it takes to go from a cold `open()` to live telemetry, and the four things
that will stop you if you get them wrong. Recovered by differential tracing of
`nvidia-smi`; the full method (shim, probe, value-in-buffer scan, per-field
command diff) is in
[nvidia-smi-clone-RE · docs/REVERSE_ENGINEERING.md](https://github.com/zagg/nvidia-smi-clone-RE/blob/main/docs/REVERSE_ENGINEERING.md).

## ABI

All ioctls use magic `'F'` (0x46), direction `Read|Write`:

```c
_IOWR('F', nr, size) = (3 << 30) | (size << 16) | ('F' << 8) | nr
```

| Escape | nr | Argument size | Purpose |
|---|---|---|---|
| `NV_ESC_CARD_INFO` | `0xc8` | 2304 = 32 × 72 | per-card table |
| `NV_ESC_REGISTER_FD` | `0xc9` | 4 | bind a `/dev/nvidiaN` fd to the control fd (arg = control fd number) |
| `NV_ESC_CHECK_VERSION_STR` | `0xd2` | 72 | version handshake |
| `NV_ESC_RM_ALLOC` | `0x2b` | 48 (`NVOS64`) | create an object |
| `NV_ESC_RM_CONTROL` | `0x2a` | 32 (`NVOS54`) | run a control |
| `NV_ESC_RM_MAP_MEMORY` | `0x4e` | 56 (`nv_ioctl_nvos33_parameters_with_fd`) | map a resource |

```c
NVOS54 { u32 hClient, hObject, cmd, flags; u64 params; u32 paramsSize, status; }   /* 32  */
NVOS64 { u32 hRoot, hObjectParent, hObjectNew, hClass; u64 pAllocParms, pRightsRequested;
         u32 paramsSize, flags, status; }                                          /* 48  */
```

`paramsSize` is part of the API: the kernel rejects any other size with a bare
`NV_ERR_INVALID_ARGUMENT` (0x1f). For undocumented controls, sweep sizes — only
the correct one returns 0.

## Sequence

```
open("/dev/nvidiactl", O_RDWR)
0xc8 CARD_INFO                       -> bus id, gpuId, IRQ, BAR windows, minor, vendor/device
0xd2 CHECK_VERSION_STR  cmd='2'      -> kernel writes ITS version string into the buffer
0xd2 CHECK_VERSION_STR  cmd=0        -> same string back: fd validated  (TRAP 1)
0x2b RM_ALLOC class 0x0041           -> hClient (NV01_ROOT_CLIENT)
0x2a 0x214 GPU_GET_PROBED_IDS (384)  -> gpuIds[32], excludedGpuIds[32], gpuFlags[32]
0x2a 0x215 GPU_ATTACH_IDS (132)      -> gpuIds[] terminated by 0xffffffff + failedId (TRAP 3)
0x2a 0x202 GPU_GET_ID_INFO (40)      -> deviceInstance, subDeviceInstance, boardId, numaId, szName
open("/dev/nvidiaN") + 0xc9 REGISTER_FD(ctl_fd)                              (TRAP 2)
0x2b RM_ALLOC class 0x0080 (NV01_DEVICE_0, 56-byte params)      -> hDevice
0x2b RM_ALLOC class 0x2080 (NV20_SUBDEVICE_0, subDeviceId)      -> hSubDevice
0x2b RM_ALLOC class 0x00de {u64 polledDataMask}                 -> hRusd      (TRAP 4)
0x4e RM_MAP_MEMORY (fd = control fd, length = 3184)             -> cookie
mmap(NULL, 3184, PROT_READ, MAP_SHARED, ctl_fd, 0)              -> telemetry page
0x2a 0xde0001 REQUEST_DATA_POLL (u64 mask)                      -> page refreshed
0x2a … one NVOS54 control per measurement
```

`NV01_DEVICE_0` and `NV20_SUBDEVICE_0` accept `paramsSize = 0` when the instance
is 0 — but this driver (615.71.09) answers `0x1f` for the device and only succeeds
with the full 56-byte `NV0080_ALLOC_PARAMETERS`. `nvtop-rs` tries both, in that
order, exactly like the C reference.

## The four traps

| Symptom | Cause | Fix |
|---|---|---|
| `0x1b NV_ERR_INSUFFICIENT_PERMISSIONS` on the `0x0080` alloc; `dmesg`: `NVRM: API mismatch: the client 'x' has the version ⟨empty⟩` | `CHECK_VERSION_STR` **validates** the string you pass; it is not a getter | QUERY (`'2'`) to learn the kernel's string, then STRICT (`0`) with it |
| the same `0x1b` even after validation | `NV01_DEVICE_0` is `RS_FLAGS_ALLOC_PRIVILEGED`: a *registered GPU fd* is also required | open `/dev/nvidiaN` and `0xc9 REGISTER_FD` with the control fd before allocating |
| `0x1f` on `0x215 ATTACH_IDS` | zero-filled `gpuIds[]` reads as invalid ids | terminate the list with `0xffffffff` |
| `0x1f` on `RM_ALLOC` class `0x00de` | `RS_REQUIRED(NV00DE_ALLOC_PARAMETERS)` | pass `{ u64 polledDataMask }` (8 bytes) |

Two more, specific to the mapping:

* `pLinearAddress` is an **mmap cookie**, not a pointer — dereferencing it
  segfaults.
* the kernel files the mapping context under the fd given in the NVOS33 wrapper's
  **`fd` field** and `nvidia_mmap_helper` requires `vm_pgoff == 0`: so `fd` must
  be the **control** fd (`/dev/nvidiaN` → `0x1f`) and the following `mmap` must
  use **`offset = 0`**, not the cookie.

## Semantics that bite

* **P-state is a bitmask** (`NV2080_CTRL_PERF_PSTATES_P2 == 0x4`), not an index:
  `4` means **P2**. Read it as an integer and you print "P4".
* **`NV0080_CTRL_GPU_SW_STATE_PERSISTENCE_ENABLED == 0`** — persistence reads
  inverted.
* **Units**: temperature `SFXP24.8` (÷256), power mW, fan duty `UFXP16.16`,
  actual clocks kHz, target clocks MHz, max clocks **10 kHz**, BAR1 and FB tags
  **KiB**.
* **KiB → MiB rounds up**: 308160 KiB is "301 MiB", not 300.
* **`used = HEAP_SIZE − HEAP_FREE`**, and `free = HEAP_FREE`; therefore
  `used + free = heap` and `total − heap = reserved`. `total − free` is wrong.
* **Absent ≠ zero**: ECC counters render `[N/A]` when ECC is off, the SM clock
  domain is unpublished (0) on this driver, memory/module power rails read 0 on
  a card without those sensors. The tool distinguishes them.
* **`RUSD_SIZE == 3184`** is the canary: it is also the length NVML itself passes
  to `mmap`. Check it first on any other driver branch.

## Status codes worth knowing

| Code | Name | Where you meet it |
|---|---|---|
| `0x00` | `NV_OK` | |
| `0x1b` | `NV_ERR_INSUFFICIENT_PERMISSIONS` | traps 1 and 2 |
| `0x1f` | `NV_ERR_INVALID_ARGUMENT` | wrong `paramsSize`, traps 3 and 4 |
| `0x33` | `NV_ERR_INVALID_OBJECT_HANDLE` | allocating a child under the wrong parent (pass `hRoot = client`, `hObjectParent = <parent>`) |
| `0x56` | `NV_ERR_NOT_SUPPORTED` | control exists, card/driver does not answer it |
