//! CoreTest 的唯一报告入口：KTAP 逐项编号、尾部计划与最终状态。
//! 每行以 `[core-test] ` 为前缀，runner 剥去前缀后校验完整性。
//! 失败由计数与用例名定位，不再受固定宽度位图限制。
use kcomp_sdk::abi::kcore_console_write_byte;

fn puts(text: &str) {
    for byte in text.bytes() {
        unsafe { kcore_console_write_byte(byte) };
    }
}

fn number(mut value: usize) {
    let mut buffer = [0; 20];
    let mut index = buffer.len();
    loop {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for &byte in &buffer[index..] {
        unsafe { kcore_console_write_byte(byte) };
    }
}

pub struct Checks {
    total: usize,
    failed: usize,
}

impl Checks {
    pub fn new() -> Self {
        puts("[core-test] KTAP version 1\n");
        Self {
            total: 0,
            failed: 0,
        }
    }

    pub fn group(&mut self, title: &str) {
        puts("[core-test] # ");
        puts(title);
        puts("\n");
    }

    pub fn check(&mut self, name: &str, ok: bool) {
        self.total += 1;
        self.failed += usize::from(!ok);
        puts(if ok {
            "[core-test] ok "
        } else {
            "[core-test] not ok "
        });
        number(self.total);
        puts(" - ");
        puts(name);
        puts("\n");
    }

    /// create 保留 0/非0 约定；每项失败的身份由报告给出。
    pub fn finish(&mut self) -> i32 {
        puts("[core-test] 1..");
        number(self.total);
        puts("\n");
        let ok = self.total > 0 && self.failed == 0;
        puts(if ok {
            "[core-test] all: PASS\n"
        } else {
            "[core-test] all: FAIL\n"
        });
        if ok { 0 } else { -1 }
    }
}
