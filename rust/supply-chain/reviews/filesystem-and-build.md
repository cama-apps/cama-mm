# Filesystem and build-context review

Exact package archives were checksum-verified against Cargo.lock. These are automated source reviews with explicit usage restrictions. They do not certify unused account-management or arbitrary inherited-descriptor APIs. The [overview](pr669.md#required-scope-and-source-guards) specifies the required deployment, feature, consumer, and source guards.

## Directory lookup

`directories 6.0.0`: all five source files and both manifests read. It contains no unsafe or privileged action; intended behavior reads HOME/XDG configuration and delegates OS directory lookup. Cama's sole consumer is the [Steam guard-data store](../../vendor/steam-vent/src/auth/guard_data.rs), using literal project names with `ProjectDirs::from`. Production supplies its own restrictive store. Directory names and environment are trusted configuration, not a pathname containment boundary.

`dirs-sys 0.5.0`: both runtime files read in full. Unix getuid/sysconf/getpwuid_r use live output storage and allocated buffers, check return codes, then copy OS-owned home bytes before storage expires. Windows known-folder allocations are freed after copying and on error. The optional user-directory parser treats configuration as data, never shell code. It can panic on a malformed one-character quote value; a test reproduces this safely. ProjectDirs does not call that parser, and Cama does not use the affected UserDirs path. No build script or filesystem mutation.

`redox_users 0.5.2`: complete runtime source read, including excluded APIs. The only reviewed use is the Redox-only dirs-sys fallback: get UID, read `AllUsers::basic(Config::default())`, find the matching user's home. Its default passwd access is read-only with Redox shared locking and checked parsing. Authentication, shadow access, login commands, account/group mutation and non-Redox pseudo-locks are excluded. Broader administration has real caveats: newline filtering, integer narrowing, inherited environment/groups and non-atomic writes require caller policy. These paths are not in Cama's supported Linux runtime, and no Redox OS execution was performed.

`libredox 0.1.23`: entire source/manifests and every unsafe site reviewed. Actual directory lookup uses scalar identity calls and errno conversion. Other pointer-bearing wrappers generally pair buffers and lengths and initialize outputs only on OS success, but raw-FD ownership, signal safety, OS ABI behavior, optional protocol/NUMA calls and OS-provided strerror UTF-8 are not globally certified. The review relies on the narrow Redox dependency path and supported Linux deployment; the optional APIs remain disabled.

## Jobserver 0.1.35

All five runtime platform files and manifests read. No build script, download, network, shell, or secret discovery. On Linux, pipe creation is CLOEXEC and checked before transferring each descriptor into File ownership. Token acquire/release uses initialized one-byte buffers; poll/FIONREAD arguments remain live and successful outputs are checked. RAII retains the owning client until token return. Helper state owns its client/callback and synchronizes through mutex/condition variables; its process-global SIGUSR1 handler is intentional build-tool behavior.

The actual consumer is `cc 1.4.2`, which retains the inherited client in a static OnceLock and uses token/helper operations. Cargo/Make environment and inherited descriptors are trusted capabilities. An environment-selected FIFO pathname is not verified to be a FIFO; untrusted MAKEFLAGS could therefore select a regular file. The separate `configure` API captures raw descriptor numbers and requires its client to remain alive through spawn; cc does not call it. Competing signal handlers, abnormal exits and stalled helper shutdown remain build-liveness limits. Windows/Wasm code was inspected, but only Linux behavior was executed.

## Test-only dependency correction

`tempfile` was listed as a normal dependency of cama-runtime-engine even though its actual shared source uses are cfg(test). It is now a dev-dependency. This removes tempfile/rustix/linux-raw-sys from that production closure while retaining their test/build review requirements; it is not an audit waiver.

Useful preliminary review covered Linux tempfile creation, permissions, no-clobber persistence, reopen identity and cleanup, plus rustix's compiler-probing build script. A complete broad production review of all rustix syscalls/assembly or generated linux-raw-sys declarations was not claimed. Their inherited safe-to-run evidence remains separate from deploy criteria.

## Validation

Four offline filesystem tests passed: ProjectDirs path handling/no creation and safe reproduction of the unused UserDirs panic; restrictive named-file permissions and no-clobber/symlink replacement behavior; libredox errno demux without FFI; and tempfile persistence/spooling. No OS account database was modified.

Isolated Linux jobserver checks passed for token RAII, a configured child returning its token, malformed/closed descriptors, missing FIFO, helper delivery/shutdown and interruption of a zero-token blocked helper. Tests created their own server and directly executed the child binary; they neither used a shell nor consumed the running Cargo jobserver's tokens. Temporary harnesses served as review evidence and are not retained CI targets.
