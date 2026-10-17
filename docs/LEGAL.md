# Legal posture

This file states, in one place, what `nvtop-rs` is, what it is not, and why that
distinction is the whole basis on which it can be published.

## 1. Independent implementation

`nvtop-rs` was written from:

1. **Facts published by the vendor.** The Linux kernel ABI of the NVIDIA driver
   — `NV_ESC_*` escape numbers, `NVOS54`/`NVOS64`/`NVOS33` structures, object
   class identifiers, RM control command identifiers and their parameter
   structures — is distributed by NVIDIA **under the MIT licence** as part of the
   *open kernel module* (`kernel-open/`, `src/common/sdk/nvidia/inc/…`). Where a
   control is documented there (`cl00de.h`, `ctrl2080fb.h`, `ctrl2080gpu.h`,
   `ctrl2080bus.h`, `nvos.h`, `nvstatuscodes.h`), this code follows those headers.
2. **Interface facts recovered by observation** on the author's own hardware,
   described in
   [nvidia-smi-clone-RE](https://github.com/zagg/nvidia-smi-clone-RE)
   (`docs/REVERSE_ENGINEERING.md`). Seven telemetry controls used here
   (`0x2080852e`, `0x2080852f`, `0x2080a0a7`, `0x2080a618`, `0x2080a61d`,
   `0x20808536`, `0x2080853a`) appear in no shipped header; their *identifiers,
   parameter sizes and the offsets/units of the values they return* are the
   result of differential tracing and probing.
3. **The vendor's own documentation and CLI output**, used only as an oracle to
   check that a recovered number matches the one displayed.

It was **not** written from:

* NVIDIA source code that is not shipped under a permissive licence;
* decompiled or disassembled code of `libnvidia-ml.so` or `nvidia-smi` — no
  disassembly is reproduced, no instruction sequence, no symbol table;
* NVIDIA firmware, ROM images, or memory dumps;
* any confidential or NDA-covered material.

The distinction matters legally and technically: an interface — *which* control
returns *which* number, in *which* unit — is a fact about how a device driver
behaves. The expression of that fact here (this Rust code, these tables, these
comments) is original. Reading the facts off a machine you own, in order to make
interoperable monitoring software, is not copying the vendor's code.

## 2. Nothing is circumvented

* **No licence check, no DRM, no protection mechanism is bypassed.** The tool
  uses the same kernel interface `nvidia-smi` uses, with the same privileges, on
  the same device nodes. There is no token, no activation, no obfuscation to
  defeat — and nothing here would help someone defeat one.
* **No elevated access is gained.** `NV01_DEVICE_0` is a privileged RM object;
  the kernel refuses it to unprivileged callers exactly as it refuses it to
  `nvidia-smi`. Running this tool requires the access you already need to run
  `nvidia-smi`.
* **No End User Licence Agreement is accepted, copied, or violated by this
  repository.** The code contains no NVIDIA code, so the NVIDIA driver EULA's
  reverse-engineering clauses have nothing here to operate on: the driver's
  *open kernel module* portion is MIT-licensed and its headers are read as the
  public specification they are. Users who run `nvtop-rs` are bound only by
  their own driver licence for the driver they installed — as with any program
  that talks to it.
* **Read-only with respect to the device.** Only `GET`/`QUERY` controls are
  issued, plus the version handshake, `ATTACH_IDS` (a client-scoped attach, not
  a device change) and object allocations that are released when the process
  exits. No `SET`/`MODIFY` control, no `NV2080_CTRL_CMD_PERF_BOOST`, no
  power-limit write, no persistence change, no reset, no firmware operation.
  Grepping the sources for `CTRL_CMD.*SET|_CTRL_.*MODIFY` returns nothing.
* **The shared telemetry page is mapped read-only** (`PROT_READ`, `MAP_SHARED`),
  and never written.

## 3. Trademarks

`NVIDIA`, `NVIDIA-SMI`, `NVML`, `CUDA`, `GeForce`, `Quadro` and `RTX` are
trademarks or registered trademarks of NVIDIA Corporation. They appear here only
to say what this software interoperates with — nominative use. This project is
**not affiliated with, connected to, sponsored or endorsed by NVIDIA
Corporation**, and it does not originate from NVIDIA. The project name
(`nvtop-rs`) and the `nv` prefix are descriptive of function; if NVIDIA's
trademark policy ever took issue with it, the rename is a `sed` away and this
file is the place that commitment is written down.

## 4. No redistribution of anything proprietary

The repository contains no binary blob, no firmware, no vendor header copy, no
screenshot of proprietary software, and no dump. The single image in `docs/` is
a rendering of **this** program's own output, captured from a pty by
`tools/capture.py`, and taken with identifying values masked (`--mask`: serial,
UUID, user names, command lines, host name).

## 5. Privacy

`--mask` (and `NVTOP_MASK=1`) redacts serial number, GPU UUID, host name, Unix
user names and process command lines everywhere they are printed, including in
`--json`. Use it before pasting a screenshot into an issue or a blog post.

## 6. No warranty

MIT-licensed software, provided as is. Reading registers through a driver can
tell you your card is 87 °C; it cannot tell you that is safe. See [LICENSE](../LICENSE).

## 7. Contributions

Contributions must follow the same rules: no pasted NVIDIA source, no disassembly
listings, no proprietary headers copied into this tree — cite the header path and
the constant name instead. Facts about behaviour (with the driver version and how
they were measured) are welcome, and are what this project is made of.
