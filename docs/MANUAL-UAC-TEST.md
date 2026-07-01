# Manually testing the Phase 17d elevated retry (UAC / polkit / osascript)

The elevated-retry **consent prompt** is the one part of Phase 17d that
can't be automated — a human has to click the OS dialog. The
`ct_elevate_probe` dev binary drives the **real**
`elevate::elevated_retry`, so you can watch the prompt fire and confirm
the elevated copy actually happens, without having to engineer a
permission-denied copy first.

> The probe is **dev-only**: it's gated behind the `elevate-probe` Cargo
> feature via `required-features`, so `cargo build`, `cargo test
> --workspace`, and the shipped app / installers never build it. Its name
> deliberately avoids the `install`/`setup`/`update`/`patch` substrings
> that would trip Windows' installer-detection auto-elevation — so the
> prompt you see comes only from our `Start-Process -Verb RunAs`.

---

## Windows

### 1. Build
```powershell
cargo build -p copythat-ui --example ct_elevate_probe --features elevate-probe
```
This drops `ct_elevate_probe.exe` next to `copythat-helper.exe` in
`target\debug\` — the probe finds the helper as its `current_exe()`
sibling, exactly as the real app does.

### 2. Run (simplest — temp files)
```powershell
.\target\debug\ct_elevate_probe.exe
```
With no args it writes a temp source file and asks the elevated helper to
copy it to a temp destination.

### 3. What to expect
1. A **UAC consent dialog** appears (publisher: *Windows PowerShell*).
   **You get this even though your account is an administrator** — Windows
   runs admins with a *split token* (a filtered, standard-user token by
   default); `Start-Process -Verb RunAs` requests the *full* elevated
   token, which always requires consent. That's expected, not a bug.
2. **Click Yes (consent):** the probe prints
   `OK — helper Response: ElevatedRetryOk { bytes: N }` and the
   destination file now exists — copied by the elevated `copythat-helper`.
3. **Click No (cancel):** the elevated child never connects, so after the
   30 s connect-timeout ceiling the probe prints
   `elevation failed/cancelled (the UI shows retry-elevated-unavailable
   here): …` and **exits — no hang.** (Cancel is normally instant; 30 s is
   only the worst case.)

### 4. Prove it really ran elevated
Point the destination at a path your normal (unelevated) account can't
write — e.g. under `C:\Program Files`:
```powershell
# create a readable source first
"proof" | Out-File -Encoding ascii C:\Windows\Temp\ct-src.bin
.\target\debug\ct_elevate_probe.exe C:\Windows\Temp\ct-src.bin "C:\Program Files\ct-elevated-proof.bin"
```
If `C:\Program Files\ct-elevated-proof.bin` appears, the helper wrote it
with the elevated token — an unelevated process would have hit *Access
Denied*. (Removing the proof file will itself need elevation.)

---

## Linux (polkit / pkexec)
```bash
cargo build -p copythat-ui --example ct_elevate_probe --features elevate-probe
./target/debug/examples/ct_elevate_probe /etc/hostname /root/ct-elevated-proof.bin
```
Expect a **polkit authentication dialog** (GUI) or a `pkexec` password
prompt (TTY). On auth, the helper runs as root and copies into `/root/…`
— a path your user can't write. Cancel/deny → `retry-elevated-unavailable`.
(Headless with no polkit agent, `elevate.rs` falls back to `sudo`; run it
from a TTY where you can type your password.)

---

## macOS (osascript admin)
```bash
cargo build -p copythat-ui --example ct_elevate_probe --features elevate-probe
./target/debug/examples/ct_elevate_probe /etc/hosts /Library/ct-elevated-proof.bin
```
Expect the system **"… wants to make changes. Enter your password to
allow this."** dialog. On auth, the helper (root) writes into `/Library/…`.
Cancel → `retry-elevated-unavailable`.

---

## What this exercises (and what it doesn't)

- **Exercises** the genuine consent flow end to end: the DACL-restricted
  named pipe (Windows) / `$XDG_RUNTIME_DIR`-or-`/tmp` 0600 socket (Unix),
  the elevated spawn, and the full
  `Hello → GrantCapabilities([ElevatedRetry]) → ElevatedRetry → Shutdown`
  handshake — i.e. the entire production `elevated_retry` path.
- **Doesn't require** an engineered permission-denied copy. In the real
  app this path fires only when an in-process copy fails with
  `err-permission-denied` and you click *Retry with elevated permissions*;
  the probe calls `elevated_retry` directly so you can verify the
  consent + copy in isolation.
