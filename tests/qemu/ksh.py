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
    # CoreTest has composed several FS providers; shell must report ambiguity.
    command("unload littlefs", ["DirectExports"])
    command("load ksh", ["load ksh: OK", "KaleidOS ksh"])
    # Let the sole shell task yield to the monitor anchor before new input.
    # The monitor must resume it rather than consume the next command itself.
    time.sleep(0.1)
    command("echo KSH_SERIAL_OK", ["\nKSH_SERIAL_OK"])
    command("help", ["load <artifact> [native|isolated]", "cat <provider-relative-path>"])
    command("components", ["KernelNative", "Ready"])
    command("endpoints", ["CONTRACT", "filesystem", "Live"])
    command("devices", ["DEVICE ID", "\n0"])
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
    command("cat 0:/HELLO.TXT", ["cat: multiple filesystem providers; selection is unavailable"])
    command("ls", ["ls: unsupported by current filesystem service"])
    command("load", ["usage: load <artifact>"])
    command("unknown", ["unknown command 'unknown'"])
    command("echo " + "x" * 130, ["input rejected: line too long"])
    command("echo KSH_RECOVERED", ["\nKSH_RECOVERED"])
    command("exit", ["ksh: exit"])
    command("unload kcomp_isolated_life", ["unload kcomp_isolated_life: OK"])
    command("unload ksh", ["unload ksh: OK"])
    return "\n".join(output)
