"""Serial user workflow smoke. Component integration assertions remain in CoreTest."""

import time


def run(proc, collect, send, failure, fatal_markers):
    output = []

    def command(line, expected):
        send(proc, line + "\n")
        text, ok = collect(proc, 30, expected, fatal_markers)
        output.append(text)
        if not ok:
            raise failure(f"ksh command {line!r}: missing {expected!r}\n" + "\n".join(output))

    # Published Direct ctx cannot be destroyed without a release protocol.
    # Standalone shell has no composed VFS; legacy FS endpoints are not a fallback.
    command("unload littlefs", ["DirectExports"])
    command("load ksh", ["load ksh: OK", "KaleidOS ksh"])
    # Let the sole shell task yield to the monitor anchor before new input.
    # The monitor must resume it rather than consume the next command itself.
    time.sleep(0.1)
    command("echo KSH_SERIAL_OK", ["\nKSH_SERIAL_OK"])
    command('echo "KSH  QUOTED" \'two words\' escaped\\ space # ignored',
            ["\nKSH  QUOTED two words escaped space"])
    command("ech\tKSH_COMPLETE_OK", ["\nKSH_COMPLETE_OK"])
    command("echo KSH_EDIT_X\x1b[D\x1b[3~OK", ["\nKSH_EDIT_OK"])
    command("\x1b[A", ["\nKSH_EDIT_OK"])
    command("echo KSH_DRAFT_OK\x1b[A\x1b[B", ["\nKSH_DRAFT_OK"])
    command("history", ["  echo KSH_EDIT_OK", "  echo KSH_DRAFT_OK"])
    command("exit\x03echo KSH_CANCEL_OK", ["^C", "\nKSH_CANCEL_OK"])
    # QEMU's stdio mux consumes Ctrl-A; doubling it forwards one byte to UART.
    command("discard\x01\x01\x0becho KSH_KILL_OK", ["\nKSH_KILL_OK"])
    command("echo KSH_CRLF_OK\r", ["\nKSH_CRLF_OK"])
    command("help", ["load <artifact> [native|isolated]", "cat <provider-relative-path>"])
    command("help exec", ["Quotes preserve each argument"])
    command("help missing", ["help: unknown command 'missing'"])
    command("components", ["KernelNative", "Ready"])
    command("endpoints", ["CONTRACT", "filesystem", "Live"])
    command("devices", ["DEVICE (PRIMARY COMPATIBLE)", "OWNER", "virtio,mmio", "ns16550a", "primary MMIO:", "IRQ[0]:", "Unclaimed", "Claimed", "core_test#", "device record(s)"])
    command("inspect ksh.kcomp", ["Format:   KCOMP (loaded component)", "Domain:   KernelNative", "State:    Ready"])
    command("load kcomp_c_smoke native", ["load kcomp_c_smoke: OK"])
    command("load kcomp_isolated_life isolated", ["load kcomp_isolated_life: OK"])
    command("inspect kcomp_isolated_life", ["Domain:   IsolatedNative", "State:    Ready"])
    # This raw ArchTest image requires a harness page unavailable to ordinary
    # lifecycle loading. Its create fault must leave only the callee Failed.
    command("load kcomp_isolated isolated", ["load kcomp_isolated: EIO"])
    command("inspect kcomp_isolated", ["Domain:   IsolatedNative", "State:    Failed"])
    command("load kcomp_panic native", ["load kcomp_panic: EIO"])
    command("inspect ksh", ["State:    Ready"])
    command("echo KSH_AFTER_LOAD_ERROR", ["\nKSH_AFTER_LOAD_ERROR"])
    command("load virtio_blk isolated", ["load virtio_blk: ENOTSUP"])
    command("load x sandboxed", ["sandboxed is unavailable"])
    command("load missing", ["load missing: ENOENT"])
    command("cat 0:/HELLO.TXT", ["cat: no filesystem provider"])
    command("ls", ["ls: unsupported by current filesystem service"])
    command("load", ["usage: load <artifact>"])
    command("exit extra", ["usage: this command takes no arguments"])
    command("unknown", ["unknown command 'unknown'"])
    command('load missing "native', ["input rejected: unclosed quote"])
    command("exit; echo prefix", ["input rejected: pipelines, redirects and command lists are unavailable"])
    command("echo " + "x" * 512, ["input rejected: line too long"])
    command("echo KSH_RECOVERED", ["\nKSH_RECOVERED"])
    command("exit", ["ksh: exit"])
    command("unload kcomp_isolated_life", ["unload kcomp_isolated_life: OK"])
    command("unload ksh", ["unload ksh: OK"])
    return "\n".join(output)
