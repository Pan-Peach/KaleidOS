#!/usr/bin/env python3
"""QEMU pty 交互测试：Core Monitor 冒烟回归。

用法: python3 tools/qemu/test_monitor.py  [kernel 路径]
默认 kernel: ./kaleidos-rv64（repo 根，make kernel 产物）

流程:
  1. pty 启动 QEMU（-nographic）
  2. 等待 boot 链完成（BOOT CORE OK + core> 提示符）
  3. 逐条发送 monitor 命令，断言语义输出（substring，不依赖精确格式）
  4. 退出码 0 = 全过；任何断言失败 / 超时 = 非 0

注意: 必须用 pty（不能管道喂 stdin）——QEMU 串口对管道输入丢字符。
"""
import pty
import os
import select
import sys
import time

KERNEL = sys.argv[1] if len(sys.argv) > 1 else "kaleidos-rv64"
QEMU = os.environ.get("QEMU", "qemu-system-riscv64")
BOOT_TIMEOUT = 20  # OpenSBI + kernel 启动上限（秒）
CMD_TIMEOUT = 3.0  # 单命令等待输出上限（秒）


def run_qemu():
    cmd = [
        QEMU, "-machine", "virt", "-smp", "2", "-m", "4G",
        "-bios", "default", "-kernel", KERNEL, "-nographic",
    ]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(QEMU, cmd)
        os._exit(1)
    return pid, fd


def read_until(fd, marker, timeout):
    """读取直到 marker 出现或超时；返回 (所有读到的字节, 是否命中)。"""
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if marker in buf:
            return buf, True
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                data = os.read(fd, 4096)
            except OSError:
                break
            if not data:
                break
            buf += data
    return buf, marker in buf


def main():
    pid, fd = run_qemu()
    try:
        # 1. 等待启动链完成
        boot, ok = read_until(fd, b"core> ", BOOT_TIMEOUT)
        if not ok:
            print(f"FAIL: 启动超时/失败，最后输出:\n{boot[-400:].decode(errors='replace')}")
            return 1
        if b"BOOT CORE OK" not in boot:
            print("FAIL: 未出现 BOOT CORE OK")
            return 1
        print("PASS: boot chain (BOOT CORE OK + core> prompt)")

        # 2. 命令断言矩阵：(命令, 期望的语义输出片段)
        checks = [
            (b"machine", b"boot hart"),          # machine 输出结构
            (b"memory", b"free frames"),         # 帧统计
            (b"frame 0x1000", b"pa 0x1000"),     # 帧查询（参数解析修复回归）
            (b"tasks", b"tasks: 0"),             # 任务表（空表初始）
            (b"help", b"tasks"),                 # help 列出全部命令（含 tasks）
            (b"bogus", b"unknown command"),      # 未知命令路径
        ]
        failed = False
        for cmd, expect in checks:
            os.write(fd, cmd + b"\r")
            time.sleep(0.6)
            out, _ = read_until(fd, b"core> ", CMD_TIMEOUT)
            ok = expect in out
            print(f"{'PASS' if ok else 'FAIL'}: {cmd.decode()} -> {expect.decode()}")
            if not ok:
                failed = True
                print(out[-300:].decode(errors="replace"))

        # 3. shutdown 结束
        os.write(fd, b"shutdown\r")
        time.sleep(1.0)

        if failed:
            return 1
        print("PASS: all monitor checks")
        return 0
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
        os.waitpid(pid, 0)


if __name__ == "__main__":
    sys.exit(main())
