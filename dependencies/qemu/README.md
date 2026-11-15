# DB2010 engine source

The `qemu` submodule pins the public QEMU `v11.1.2` commit
`4fc49f46dc95d4a27de2509e7fceb2931e91faeb`. All DB2010 board code and subsequent
emulation changes are included in `patches/`, in the order recorded in
`source-lock.json`. Patch 0000 contains the original DB2010 port, including its
decoder sources and licenses. Later patches extend that port.

The previous local fork revision is recorded as `original_fork_commit` for
provenance. Publication changes the source packaging; the resulting emulation
source bytes are identical. Every patch and its output files have SHA-256
identities. Build scripts apply and verify the series and preserve unrelated
local modifications.

Initialize the submodules with `git submodule update --init --recursive`.
`frontends/build_desktop.py` and the Android native build scripts apply the
authenticated patches automatically. The Python research runner can initialize
and check them with `python -m db2010.development.source init`.
