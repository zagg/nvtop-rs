# Data sources

Every number `nvtop-rs` prints, with the command it came from, its offset and its
unit. Undocumented = present in no header NVIDIA ships; those were decoded by
probing (method: sibling repo, `docs/REVERSE_ENGINEERING.md` §9).

Privileges: the process must be able to open `/dev/nvidiactl` and
`/dev/nvidiaN` and to allocate `NV01_DEVICE_0` — i.e. what `nvidia-smi` needs.

## Identity and driver

| Field | Source | Notes |
|---|---|---|
| model name | `0x20800110` (68 B) | `char name[64]` at +4 |
| marketing name, serial, part numbers, board rev | `0x2080013f` (136 B) | EEPROM: buildDate@0, marketing[24]@4, serial[16]@28, memMfr@44, memPart[20]@45, memDate[6]@65, productPart[20]@71, boardRev[3]@91, boardType@94, 699[20]@95, 965[20]@115 |
| UUID | `0x2080014a` (268 B) | IN `{index 0, flags 2}` → 16 raw bytes at +12, length at +8 |
| VBIOS | `/proc/driver/nvidia/gpus/<bdf>/information` | key `Video BIOS` (the RM control `0x20800802` is pointer-based) |
| InfoROM versions | `0x2080014b` (8 B) | IN `char objectType[3]` ∈ `img`,`cap`,`rsd` → version/subversion |
| architecture, implementation, chip rev | `0x20801701` (16 B) | GA104 = arch `0x170`, impl 4 → compute capability 8.6 |
| ECID, capabilities (SM version, ATS, FLA, DMA-BUF, MIG-resetless, …) | `0x20800102` `GPU_GET_INFO_V2` (580 B) | indexed list `{u32 index, u32 data}`, 27 indices |
| engines | `0x20800170` (340 B) | `NV2080_ENGINE_TYPE_*` |
| device / subdevice instance, boardId, gpuInstance, NUMA, RM name | `0x202` `GPU_GET_ID_INFO` (40 B) | `szName` is a pointer we hand over |
| driver version, RM branch, build title, changelist | `0x101` `SYSTEM_GET_BUILD_VERSION` (40 B) | three 255-byte output buffers |
| KMD version | `0xd2` `CHECK_VERSION_STR` with `cmd='2'` | the *only* self-contained way to learn it before the fd is validated |
| persistence | `0x800288` (4 B) on the device | **inverted**: `0 == Enabled` |
| compute mode | `0x20800131` (4 B) | 0 Default, 1 Excl. Thread, 2 Prohibited, 3 Exclusive_Process |
| ECC mode | `0x20800133` (8 B) | `{bEnable, bEnabledOnNextBoot}` |
| virtualisation mode | `0x800289` (8 B) | |
| display heads / "screen connected" | class `0x0073` + `0x730102`, `0x73010c` | `displayId != 0` on any head |
| CUDA UMD version | `dlopen("libcuda.so.1")` → `cuDriverGetVersion` | **not in the driver**; opt-out `--no-cuda` |

## Bus

| Field | Source | Notes |
|---|---|---|
| domain / bus / slot / function, gpuId, IRQ, BAR0/BAR1 windows, minor | `0xc8` `CARD_INFO` | `nv_ioctl_card_info`, 72 B × 32 |
| vendor/device, subsystem, class, revision | `/sys/bus/pci/devices/<bdf>/config` + `CARD_INFO` | names resolved from `/usr/share/misc/pci.ids` when present |
| link gen/width (current and max), max payload, ASPM, port type | same config space, PCIe capability `0x10` | `LNKCAP +0x0C`, `LNKCTL +0x10`, `LNKSTA +0x12` |
| power state D0/D3 | PM capability `0x01` | |
| AER counters, `current_link_speed`, NUMA node, `local_cpulist`, IOMMU group, MSI list, BAR sizes and flags, runtime PM | `/sys/bus/pci/devices/<bdf>/{aer_dev_*,current_link_speed,numa_node,local_cpulist,iommu_group,msi_irqs,resource,power/runtime_status}` | |
| link errors (8 counters) + `PCIE_GEN_INFO` | RUSD `pciBusData[9]` (`@2536`) and `0x20801823` `BUS_GET_INFO_V2` | gen info: max bits 15:12, current bits 19:16, both +1 |

## Memory

| Field | Source | Unit / conversion |
|---|---|---|
| total, heap, free, largest free block, BAR1, RAM type, bus width, L2, partitions, LTC/LTS, vendor, protected memory, NUMA id, offline/reclaimable heap | `0x20801303` `FB_GET_INFO_V2` (1028 B), 31 tags | **KiB**, converted to MiB **rounded up** (NVML behaviour) |
| used | tags `HEAP_SIZE(9) − HEAP_FREE(22)` | not `total − free`: `used + free = heap`, `total − heap = reserved` |
| BAR1 total/free | RUSD `bar1MemoryInfo` (`@0`) **or** tags 5 / `0x1d` | KiB |
| PMA pool | RUSD `pmaMemoryInfo` (`@16`) | bytes |

## Live telemetry — one `0xde0001` poll, then memory

The page is `NV00DE_SHARED_DATA`, class `0x00de`, exactly **3184 bytes**, mapped
`PROT_READ` from the **control** fd with `mmap(offset = 0)`. Each section is read
under its seqlock: read the 8-byte timestamp, copy, re-read; valid if equal and
neither `0` (invalid) nor `~0` (write in flight).

| Offset | Section | Used for | Unit |
|---|---|---|---|
| 0 | `bar1MemoryInfo` | BAR1 | KiB |
| 16 | `pmaMemoryInfo` | PMA total/free | bytes |
| 40 | `shadowErrCont` | shadowed `ERR_CONT` register | raw |
| 56 | `grInfo` | context-switch logging flag | bool |
| 72 | `clkPublicDomainInfos` | 4 × {target MHz, actual kHz} → graphics, memory, video, SM | kHz→MHz; **SM unpublished (0) ⇒ fall back to graphics** |
| 112 | `clkThrottleInfo` | reason mask @+8, referenceTimeNs @+16, supportedViolationTimeMask @+24, 32 × `violation` @+32 (56 B each), `globalViolation` @+1824 | mask / ns |
| 1992 | `perfDevUtil` | GPU %, memory %, 4 × {busy, period} = NVENC/NVDEC/NVJPG/NVOFA | %, µs |
| 2040 | `memEcc` | 3 groups (TOTAL, DRAM, SRAM) × 4 × u64, then `volatileCounts` (5 × u64 @+104) and `clientExposedCounts` (@+144: 5 × u64, lastCleared u32, 5 buckets u64, threshold bool) | errors |
| 2280 | `perfCurrentPstate` | P-state | **bitmask**: `4` ⇒ P2 (`trailing_zeros`) |
| 2296 | `powerLimitGpu` | requested / enforced limit | mW |
| 2312 | `temperatures[5]` | GPU, memory, board, power supply, HBM | **SFXP24.8** (÷256) |
| 2392 | `memRowRemap` | 5-bin histogram, correctable/uncorrectable (+inactive), pending, failure-occurred | rows |
| 2440 | `memBankRemap` | spare group counts, pending, active/inactive | banks |
| 2472 | `avgPowerUsage` | GPU / module / memory | mW |
| 2496 | `instPowerUsage` | GPU / module / CPU | mW |
| 2520 | `powerPolicyStatus` | TGPM | mW |
| 2536 | `pciBusData` | gen info + 8 link error counters | counts |
| 2584 | `fanCoolerStatus` | `rpmCurr[16]` | RPM |
| 2656 | `procUtil` | ring of 5 × {timestamp, 6 engines × {util, vgpuScale, pid, subPid}} | %×100, PID |

Poll mask bits: `0x01` clock, `0x02` perf, `0x04` memory, `0x08` power,
`0x10` thermal, `0x20` pci, `0x40` fan, `0x80` proc-util (`RUSD_POLL_ALL = 0xff`).

## Power, thermals, clocks (RM controls)

| Field | Source | Notes |
|---|---|---|
| fan duty % | `0x2080852f` (844 B), IN `{0, 1}` — **undocumented** | reply +32 is **UFXP16.16** (`0xa88b` → 65.8 %) |
| sensor/cooler table (arms the reading above) | `0x2080852e` (1432 B) — **undocumented** | called once at startup |
| current power limit | `0x2080a61d` (12 B), IN selector `0xfe01` — **undocumented** | mW at +4 |
| default power limit | `0x2080a618` (20512 B) — **undocumented** | mW at +736 |
| max graphics/SM clock | `0x2080a0a7` (17676 B) — **undocumented** | **10 kHz** units at +36 |
| P-state (cross-check) | `0x20802068` | mask, same decoding as RUSD |
| power state | `0x2080205a` | |
| rated TDP status/level | `0x2080206e` (12 B) | |
| ECC live counters (cross-check) | `0x20803400` (96 B), `0x20803401` (40 B) | |

## Processes

| Field | Source | Notes |
|---|---|---|
| PID list | `0x2080018d` (3812 B), IN `{idType 0, id = channel class}` | `0xc7c0` → compute (**C**), `0xc797` → 3D (**G**) |
| memory per PID, MIG GI/CI | `0x2080018e` (14408 B), 72-byte records | memPrivate@16, memSharedOwned@24, memSharedDuped@32, protected@40/48/56, GI@64, CI@68 — bytes |
| command line, exe, user, RSS, threads, CPU core, state | `/proc/<pid>/{cmdline,exe,status,stat}` | CPU % from utime+stime deltas between frames |
| per-engine occupancy per PID | RUSD `procUtil` | max over the ring, per engine |

## Not obtained anywhere

| Field | Status |
|---|---|
| MIG mode | `0x20810108` on `NV2081_BINAPI`: protobuf, schema not shipped |
| `clocks.max.memory`, `power.min/max_limit` | not located in any blob pierced so far (leads in the sibling repo §8) |
| NVLink, C2C, USB-C, PMU timestamps | no such hardware on the testbed |
