//! 行编辑器：纯状态机 + 屏幕 sink —— 无 I/O、无堆分配。
//!
//! `feed` 每次只吃一个字节（输入端是轮询 `Console::getc`），返回值告诉驱动
//! 这一行何时提交/取消/EOF。所有状态（含 ESC 序列解析）跨 `feed` 调用保持。
//!
//! 渲染只用 `\r` / 退格 / 空格（无 ANSI），唯一的 ANSI 出口是 Ctrl-L 清屏。
//! 行缓冲与历史环都是固定数组 —— 目标路径零堆分配。

/// 行缓冲上限：提示符 `"core> "`（6 列）+ 72 ≤ 80 列（不处理折行）。
pub const LINE_MAX: usize = 72;

/// 历史环容量。
pub const HISTORY_MAX: usize = 8;

const BELL: u8 = 0x07;
const BACKSPACE: u8 = 0x08;
/// 唯一的 ANSI 出口：Ctrl-L 清屏 + 光标归位。
const CLEAR_SCREEN: &[u8] = b"\x1b[2J\x1b[H";
const SPACES: [u8; LINE_MAX] = [b' '; LINE_MAX];
const BACKSPACES: [u8; LINE_MAX] = [BACKSPACE; LINE_MAX];

/// 屏幕 sink：编辑器只通过它输出字节（驱动接 Console；测试接 `VecScreen`）。
pub trait Screen {
    fn put(&mut self, bytes: &[u8]);
}

/// 一次 `feed` 的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 行还没结束，继续喂字节。
    Pending,
    /// 回车：`line()` 是刚提交的内容，直到下一个字节到来前都有效。
    Submitted,
    /// Ctrl-C：行被丢弃。
    Cancelled,
    /// Ctrl-D 且缓冲为空：调用方决定怎么处理（monitor 忽略、read_line 返回 0）。
    Eof,
}

/// ESC 序列解析状态（跨 `feed` 保持：字节是逐个到的）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EscState {
    Ground,
    /// 刚收到 ESC。
    Esc,
    /// ESC `[`：CSI 参数序列。
    Csi,
    /// ESC `O`：SS3 单字节序列。
    Ss3,
}

/// 纯行编辑器：固定数组 + 状态机，无 I/O、无堆。
pub struct LineEditor {
    buf: [u8; LINE_MAX],
    len: usize,
    cursor: usize,
    /// 上一次渲染在提示符后显示的字节数（用于增量擦除）。
    shown: usize,
    esc: EscState,
    /// CSI 数字参数（只认第一个；`ESC [ 3 ~`）。
    csi_param: u16,
    /// 参数目前是否全为数字（出现 `;` / 中间字节即失效）。
    csi_param_valid: bool,
    /// 刚结束一行：下一个字节先清理缓冲，并吞掉 CRLF 的另一半。
    just_submitted: bool,
    // —— 历史环：槽位固定，`history_next` 是下一个写入槽 ——
    history: [[u8; LINE_MAX]; HISTORY_MAX],
    history_len: [usize; HISTORY_MAX],
    history_count: usize,
    history_next: usize,
    /// `Some(i)`：正在浏览第 i 条历史（0 = 最近）；`None` = 编辑当前行。
    history_pos: Option<usize>,
    /// 浏览历史前暂存的"进行中"行（Down 回到 None 时恢复）。
    stash: [u8; LINE_MAX],
    stash_len: usize,
}

impl LineEditor {
    pub const fn new() -> Self {
        Self {
            buf: [0; LINE_MAX],
            len: 0,
            cursor: 0,
            shown: 0,
            esc: EscState::Ground,
            csi_param: 0,
            csi_param_valid: false,
            just_submitted: false,
            history: [[0; LINE_MAX]; HISTORY_MAX],
            history_len: [0; HISTORY_MAX],
            history_count: 0,
            history_next: 0,
            history_pos: None,
            stash: [0; LINE_MAX],
            stash_len: 0,
        }
    }

    /// 当前编辑缓冲（提交后到下一次 `feed` 前 = 刚提交的那一行）。
    pub fn line(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// 处理一个输入字节。`prompt` 在重绘时重新输出；`cands` 是首个 token 的
    /// 补全候选（`&[]` 关闭补全）。
    pub fn feed(
        &mut self,
        prompt: &str,
        byte: u8,
        cands: &[&str],
        screen: &mut dyn Screen,
    ) -> Outcome {
        // 上一行刚结束：先清理缓冲（提交的行此时已被驱动读过），
        // 若是 CRLF 的另一半则直接吞掉。
        if self.just_submitted {
            self.just_submitted = false;
            let crlf_tail = matches!(byte, b'\r' | b'\n');
            self.reset_line();
            if crlf_tail {
                return Outcome::Pending;
            }
        }

        // ESC 序列中的字节不参与普通编辑（未识别的序列整体吞掉）。
        if self.esc != EscState::Ground {
            return self.feed_escape(prompt, byte, screen);
        }

        match byte {
            b'\r' | b'\n' => self.submit(screen),
            0x03 => self.cancel(screen),
            0x04 => {
                if self.len == 0 {
                    Outcome::Eof
                } else {
                    Outcome::Pending
                }
            }
            0x08 | 0x7f => {
                self.delete_before_cursor(prompt, screen);
                Outcome::Pending
            }
            0x01 => {
                self.cursor = 0;
                self.redraw(prompt, screen);
                Outcome::Pending
            }
            0x05 => {
                self.cursor = self.len;
                self.redraw(prompt, screen);
                Outcome::Pending
            }
            0x02 => {
                self.move_left(prompt, screen);
                Outcome::Pending
            }
            0x06 => {
                self.move_right(prompt, screen);
                Outcome::Pending
            }
            0x0b => {
                self.kill_to_end(prompt, screen);
                Outcome::Pending
            }
            0x0c => {
                screen.put(CLEAR_SCREEN);
                self.shown = 0;
                self.redraw(prompt, screen);
                Outcome::Pending
            }
            0x15 => {
                self.kill_to_start(prompt, screen);
                Outcome::Pending
            }
            0x17 => {
                self.kill_word(prompt, screen);
                Outcome::Pending
            }
            0x09 => {
                self.complete(prompt, cands, screen);
                Outcome::Pending
            }
            0x1b => {
                self.esc = EscState::Esc;
                Outcome::Pending
            }
            // 其余 C0 控制字节：忽略，绝不插入（0x00–0x1f 中未列出的）。
            0x00..=0x1f => Outcome::Pending,
            // 可打印 ASCII 与 >= 0x80 的字节：插入光标处。
            _ => {
                self.insert(byte, prompt, screen);
                Outcome::Pending
            }
        }
    }

    // ------------------------------------------------------------------
    // 行结束
    // ------------------------------------------------------------------

    fn submit(&mut self, screen: &mut dyn Screen) -> Outcome {
        self.push_history();
        screen.put(b"\r\n");
        self.just_submitted = true;
        self.shown = 0;
        self.history_pos = None;
        self.stash_len = 0;
        Outcome::Submitted
    }

    fn cancel(&mut self, screen: &mut dyn Screen) -> Outcome {
        screen.put(b"^C\r\n");
        self.reset_line();
        self.history_pos = None;
        self.stash_len = 0;
        Outcome::Cancelled
    }

    /// 丢弃当前行状态（提交/取消后的下一次 `feed` 调用）。
    fn reset_line(&mut self) {
        self.len = 0;
        self.cursor = 0;
        self.shown = 0;
    }

    // ------------------------------------------------------------------
    // 编辑动作
    // ------------------------------------------------------------------

    fn insert(&mut self, byte: u8, prompt: &str, screen: &mut dyn Screen) {
        if self.len >= LINE_MAX {
            screen.put(&[BELL]);
            return;
        }
        if self.cursor < self.len {
            self.buf.copy_within(self.cursor..self.len, self.cursor + 1);
        }
        self.buf[self.cursor] = byte;
        self.len += 1;
        self.cursor += 1;
        self.redraw(prompt, screen);
    }

    fn delete_before_cursor(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor == 0 {
            return;
        }
        self.buf.copy_within(self.cursor..self.len, self.cursor - 1);
        self.len -= 1;
        self.cursor -= 1;
        self.redraw(prompt, screen);
    }

    fn delete_at_cursor(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor >= self.len {
            return;
        }
        self.buf.copy_within(self.cursor + 1..self.len, self.cursor);
        self.len -= 1;
        self.redraw(prompt, screen);
    }

    fn move_left(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.redraw(prompt, screen);
        }
    }

    fn move_right(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor < self.len {
            self.cursor += 1;
            self.redraw(prompt, screen);
        }
    }

    fn kill_to_start(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor == 0 {
            return;
        }
        self.buf.copy_within(self.cursor..self.len, 0);
        self.len -= self.cursor;
        self.cursor = 0;
        self.redraw(prompt, screen);
    }

    fn kill_to_end(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.cursor >= self.len {
            return;
        }
        self.len = self.cursor;
        self.redraw(prompt, screen);
    }

    fn kill_word(&mut self, prompt: &str, screen: &mut dyn Screen) {
        let mut start = self.cursor;
        while start > 0 && self.buf[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.buf[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        if start == self.cursor {
            return;
        }
        self.buf.copy_within(self.cursor..self.len, start);
        self.len -= self.cursor - start;
        self.cursor = start;
        self.redraw(prompt, screen);
    }

    // ------------------------------------------------------------------
    // 历史
    // ------------------------------------------------------------------

    fn push_history(&mut self) {
        if self.len == 0 {
            return;
        }
        if self.history_count > 0 {
            let last = (self.history_next + HISTORY_MAX - 1) % HISTORY_MAX;
            if self.history_len[last] == self.len
                && self.history[last][..self.len] == self.buf[..self.len]
            {
                return;
            }
        }
        let slot = self.history_next;
        self.history[slot][..self.len].copy_from_slice(&self.buf[..self.len]);
        self.history_len[slot] = self.len;
        self.history_next = (self.history_next + 1) % HISTORY_MAX;
        if self.history_count < HISTORY_MAX {
            self.history_count += 1;
        }
    }

    fn history_older(&mut self, prompt: &str, screen: &mut dyn Screen) {
        if self.history_count == 0 {
            return;
        }
        match self.history_pos {
            None => {
                self.stash_len = self.len;
                self.stash[..self.len].copy_from_slice(&self.buf[..self.len]);
                self.history_pos = Some(0);
            }
            Some(pos) => {
                if pos + 1 >= self.history_count {
                    return;
                }
                self.history_pos = Some(pos + 1);
            }
        }
        if let Some(pos) = self.history_pos {
            let slot = (self.history_next + HISTORY_MAX - 1 - pos) % HISTORY_MAX;
            let len = self.history_len[slot];
            self.buf[..len].copy_from_slice(&self.history[slot][..len]);
            self.len = len;
            self.cursor = len;
            self.redraw(prompt, screen);
        }
    }

    fn history_newer(&mut self, prompt: &str, screen: &mut dyn Screen) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos == 0 {
            // 回到浏览前的"进行中"行。
            let len = self.stash_len;
            self.buf[..len].copy_from_slice(&self.stash[..len]);
            self.len = len;
            self.cursor = len;
            self.history_pos = None;
        } else {
            let older = pos - 1;
            self.history_pos = Some(older);
            let slot = (self.history_next + HISTORY_MAX - 1 - older) % HISTORY_MAX;
            let len = self.history_len[slot];
            self.buf[..len].copy_from_slice(&self.history[slot][..len]);
            self.len = len;
            self.cursor = len;
        }
        self.redraw(prompt, screen);
    }

    // ------------------------------------------------------------------
    // Tab 补全（只作用于首个 token）
    // ------------------------------------------------------------------

    fn complete(&mut self, prompt: &str, cands: &[&str], screen: &mut dyn Screen) {
        let mut start = 0;
        while start < self.len && self.buf[start].is_ascii_whitespace() {
            start += 1;
        }
        let mut end = start;
        while end < self.len && !self.buf[end].is_ascii_whitespace() {
            end += 1;
        }

        let mut prefix = [0u8; LINE_MAX];
        let prefix_len = end - start;
        prefix[..prefix_len].copy_from_slice(&self.buf[start..end]);

        let mut matches = 0usize;
        let mut only: Option<&str> = None;
        let mut common = [0u8; LINE_MAX];
        let mut common_len = 0usize;
        for cand in cands {
            let bytes = cand.as_bytes();
            if !bytes.starts_with(&prefix[..prefix_len]) {
                continue;
            }
            matches += 1;
            only = Some(cand);
            if matches == 1 {
                common_len = bytes.len();
                common[..common_len].copy_from_slice(bytes);
            } else {
                let mut same = 0usize;
                while same < common_len && same < bytes.len() && common[same] == bytes[same] {
                    same += 1;
                }
                common_len = same;
            }
        }

        match matches {
            0 => screen.put(&[BELL]),
            1 => {
                let Some(cand) = only else {
                    return;
                };
                self.replace_token(start, end, cand.as_bytes(), true, prompt, screen);
            }
            _ => {
                if common_len > prefix_len {
                    self.replace_token(start, end, &common[..common_len], false, prompt, screen);
                } else {
                    // 没有可扩展的公共前缀：打印候选列表再重绘。
                    screen.put(b"\r\n");
                    let mut first = true;
                    for cand in cands {
                        if !cand.as_bytes().starts_with(&prefix[..prefix_len]) {
                            continue;
                        }
                        if !first {
                            screen.put(b"  ");
                        }
                        screen.put(cand.as_bytes());
                        first = false;
                    }
                    screen.put(b"\r\n");
                    self.shown = 0;
                    self.redraw(prompt, screen);
                }
            }
        }
    }

    /// 用 `replacement` 替换 `[start, end)`；`append_space` 时再补一个空格。
    fn replace_token(
        &mut self,
        start: usize,
        end: usize,
        replacement: &[u8],
        append_space: bool,
        prompt: &str,
        screen: &mut dyn Screen,
    ) {
        let added = replacement.len() + usize::from(append_space);
        let new_len = self.len - (end - start) + added;
        if new_len > LINE_MAX {
            screen.put(&[BELL]);
            return;
        }
        self.buf.copy_within(end..self.len, start + added);
        self.buf[start..start + replacement.len()].copy_from_slice(replacement);
        if append_space {
            self.buf[start + replacement.len()] = b' ';
        }
        self.len = new_len;
        self.cursor = start + added;
        self.redraw(prompt, screen);
    }

    // ------------------------------------------------------------------
    // ESC 序列
    // ------------------------------------------------------------------

    fn feed_escape(&mut self, prompt: &str, byte: u8, screen: &mut dyn Screen) -> Outcome {
        match self.esc {
            EscState::Esc => match byte {
                b'[' => {
                    self.esc = EscState::Csi;
                    self.csi_param = 0;
                    self.csi_param_valid = true;
                }
                b'O' => self.esc = EscState::Ss3,
                // 连续 ESC：重新开始（不插入任何字节）。
                0x1b => {}
                // 未识别：整体吞掉。
                _ => self.esc = EscState::Ground,
            },
            EscState::Csi => match byte {
                b'0'..=b'9' => {
                    if self.csi_param_valid {
                        self.csi_param = self
                            .csi_param
                            .saturating_mul(10)
                            .saturating_add(u16::from(byte - b'0'));
                    }
                }
                // 参数分隔 / 中间字节：本实现只认单数字参数，标记失效。
                b';' | 0x20..=0x2f => self.csi_param_valid = false,
                _ => {
                    let param = self.csi_param;
                    let valid = self.csi_param_valid;
                    self.esc = EscState::Ground;
                    self.csi_param = 0;
                    self.csi_param_valid = false;
                    match (byte, valid, param) {
                        (b'A', true, 0) => self.history_older(prompt, screen),
                        (b'B', true, 0) => self.history_newer(prompt, screen),
                        (b'C', true, 0) => self.move_right(prompt, screen),
                        (b'D', true, 0) => self.move_left(prompt, screen),
                        (b'H', true, 0) => {
                            self.cursor = 0;
                            self.redraw(prompt, screen);
                        }
                        (b'F', true, 0) => {
                            self.cursor = self.len;
                            self.redraw(prompt, screen);
                        }
                        (b'~', true, 3) => self.delete_at_cursor(prompt, screen),
                        _ => {}
                    }
                }
            },
            EscState::Ss3 => {
                self.esc = EscState::Ground;
                match byte {
                    b'A' => self.history_older(prompt, screen),
                    b'B' => self.history_newer(prompt, screen),
                    b'C' => self.move_right(prompt, screen),
                    b'D' => self.move_left(prompt, screen),
                    b'H' => {
                        self.cursor = 0;
                        self.redraw(prompt, screen);
                    }
                    b'F' => {
                        self.cursor = self.len;
                        self.redraw(prompt, screen);
                    }
                    _ => {}
                }
            }
            EscState::Ground => self.esc = EscState::Ground,
        }
        Outcome::Pending
    }

    // ------------------------------------------------------------------
    // 渲染：`\r` + prompt + buf + 擦尾空格 + 光标回退（无 ANSI）
    // ------------------------------------------------------------------

    fn redraw(&mut self, prompt: &str, screen: &mut dyn Screen) {
        screen.put(b"\r");
        screen.put(prompt.as_bytes());
        screen.put(&self.buf[..self.len]);
        let spaces = self.shown.saturating_sub(self.len);
        if spaces > 0 {
            screen.put(&SPACES[..spaces]);
        }
        let back = self.shown.max(self.len) - self.cursor;
        if back > 0 {
            screen.put(&BACKSPACES[..back]);
        }
        self.shown = self.len;
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

/// 记录全部输出字节的测试 sink（仅 host 测试编译）。
#[cfg(test)]
#[derive(Default)]
pub struct VecScreen {
    pub bytes: alloc::vec::Vec<u8>,
}

#[cfg(test)]
impl VecScreen {
    pub fn new() -> Self {
        Self {
            bytes: alloc::vec::Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }
}

#[cfg(test)]
impl Screen for VecScreen {
    fn put(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_editor() -> (LineEditor, VecScreen) {
        (LineEditor::new(), VecScreen::new())
    }

    /// 逐字节敲入（测试提示符统一 `p> `）。
    fn type_text(ed: &mut LineEditor, text: &str) {
        let mut screen = VecScreen::new();
        for byte in text.bytes() {
            assert_eq!(ed.feed("p> ", byte, &[], &mut screen), Outcome::Pending);
        }
    }

    fn submit_text(ed: &mut LineEditor, text: &str) {
        type_text(ed, text);
        let mut screen = VecScreen::new();
        assert_eq!(ed.feed("p> ", b'\r', &[], &mut screen), Outcome::Submitted);
    }

    fn feed_seq(ed: &mut LineEditor, seq: &[u8]) {
        let mut screen = VecScreen::new();
        for &byte in seq {
            assert_eq!(ed.feed("p> ", byte, &[], &mut screen), Outcome::Pending);
        }
    }

    fn feed_byte(ed: &mut LineEditor, byte: u8) -> Outcome {
        let mut screen = VecScreen::new();
        ed.feed("p> ", byte, &[], &mut screen)
    }

    fn feed_into(ed: &mut LineEditor, screen: &mut VecScreen, byte: u8) -> Outcome {
        ed.feed("p> ", byte, &[], screen)
    }

    // -- ESC 序列 ---------------------------------------------------------

    #[test]
    fn escape_sequences_insert_nothing_and_are_swallowed() {
        let (mut ed, mut screen) = new_editor();
        let sequences: &[&[u8]] = &[
            b"\x1b[A",
            b"\x1b[B",
            b"\x1b[C",
            b"\x1b[D",
            b"\x1b[H",
            b"\x1b[F",
            b"\x1bOA",
            b"\x1bOB",
            b"\x1bOC",
            b"\x1bOD",
            b"\x1bOH",
            b"\x1bOF",
            b"\x1b[3~",
            b"\x1b[1;5C",
            b"\x1b[99Z",
            b"\x1bZ",
        ];
        for seq in sequences {
            for &byte in *seq {
                assert_eq!(ed.feed("p> ", byte, &[], &mut screen), Outcome::Pending);
            }
        }
        assert_eq!(ed.line(), b"", "ESC 序列的字节绝不能进缓冲");
        assert!(!screen.bytes.contains(&0x1b), "ESC 字节不得泄漏到屏幕输出");
    }

    #[test]
    fn arrow_left_moves_cursor_without_inserting() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "ab");
        feed_seq(&mut ed, b"\x1b[D");
        type_text(&mut ed, "X");
        assert_eq!(ed.line(), b"aXb", "左箭头只移动光标，不插入 ESC[D");
    }

    #[test]
    fn arrow_right_and_history_sequences_do_not_insert() {
        let (mut ed, _) = new_editor();
        submit_text(&mut ed, "one");
        type_text(&mut ed, "x");
        feed_seq(&mut ed, b"\x1b[A"); // Up → "one"
        assert_eq!(ed.line(), b"one");
        feed_seq(&mut ed, b"\x1b[B"); // Down → 恢复 "x"
        assert_eq!(ed.line(), b"x");
        feed_seq(&mut ed, b"\x1b[C");
        type_text(&mut ed, "y");
        assert_eq!(ed.line(), b"xy");
    }

    #[test]
    fn esc_delete_at_cursor_removes_current_char() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "abc");
        feed_seq(&mut ed, b"\x1b[H");
        feed_seq(&mut ed, b"\x1b[3~");
        assert_eq!(ed.line(), b"bc");
        feed_seq(&mut ed, b"\x1b[4~"); // 未识别序列：吞掉
        assert_eq!(ed.line(), b"bc");
    }

    // -- 光标与删除 -------------------------------------------------------

    #[test]
    fn backspace_at_start_is_noop_and_deletes_before_cursor() {
        let (mut ed, mut screen) = new_editor();
        assert_eq!(ed.feed("p> ", 0x08, &[], &mut screen), Outcome::Pending);
        assert_eq!(ed.feed("p> ", 0x7f, &[], &mut screen), Outcome::Pending);
        assert!(screen.bytes.is_empty(), "行首退格不得产生重绘");

        type_text(&mut ed, "ab");
        feed_byte(&mut ed, 0x7f);
        assert_eq!(ed.line(), b"a");
        feed_byte(&mut ed, 0x08);
        assert_eq!(ed.line(), b"");
        feed_byte(&mut ed, 0x08);
        assert_eq!(ed.line(), b"");
    }

    #[test]
    fn insert_at_cursor_shifts_tail() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "ac");
        feed_byte(&mut ed, 0x02); // Ctrl-B → 光标移到 'a' 与 'c' 之间
        type_text(&mut ed, "b");
        assert_eq!(ed.line(), b"abc");
    }

    #[test]
    fn ctrl_a_home_and_ctrl_e_end() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "bc");
        feed_byte(&mut ed, 0x01); // Ctrl-A
        type_text(&mut ed, "a");
        assert_eq!(ed.line(), b"abc");
        feed_byte(&mut ed, 0x05); // Ctrl-E
        type_text(&mut ed, "d");
        assert_eq!(ed.line(), b"abcd");
    }

    #[test]
    fn esc_home_and_end_sequences_move_cursor() {
        // ESC [ H / ESC [ F
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "bc");
        feed_seq(&mut ed, b"\x1b[H");
        type_text(&mut ed, "a");
        feed_seq(&mut ed, b"\x1b[F");
        type_text(&mut ed, "d");
        assert_eq!(ed.line(), b"abcd");

        // ESC O H / ESC O F
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "bc");
        feed_seq(&mut ed, b"\x1bOH");
        type_text(&mut ed, "a");
        feed_seq(&mut ed, b"\x1bOF");
        type_text(&mut ed, "d");
        assert_eq!(ed.line(), b"abcd");
    }

    #[test]
    fn ctrl_u_kills_from_start_to_cursor() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "abc");
        feed_byte(&mut ed, 0x02); // 光标在 'c' 前
        feed_byte(&mut ed, 0x15); // Ctrl-U
        assert_eq!(ed.line(), b"c");
    }

    #[test]
    fn ctrl_k_kills_from_cursor_to_end() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "abc");
        feed_byte(&mut ed, 0x01); // home
        feed_byte(&mut ed, 0x06); // 光标到 'b'
        feed_byte(&mut ed, 0x0b); // Ctrl-K
        assert_eq!(ed.line(), b"a");
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "foo bar");
        feed_byte(&mut ed, 0x17); // Ctrl-W
        assert_eq!(ed.line(), b"foo ");
        type_text(&mut ed, "baz");
        feed_byte(&mut ed, 0x17);
        assert_eq!(ed.line(), b"foo ");
        feed_byte(&mut ed, 0x17); // 再吃一个词（含尾部空格）
        assert_eq!(ed.line(), b"");
    }

    // -- 历史 -------------------------------------------------------------

    #[test]
    fn history_up_down_restores_stash() {
        let (mut ed, _) = new_editor();
        submit_text(&mut ed, "one");
        submit_text(&mut ed, "two");
        type_text(&mut ed, "dr");

        feed_seq(&mut ed, b"\x1b[A");
        assert_eq!(ed.line(), b"two", "Up 从最近一条开始");
        feed_seq(&mut ed, b"\x1b[A");
        assert_eq!(ed.line(), b"one");
        feed_seq(&mut ed, b"\x1b[A");
        assert_eq!(ed.line(), b"one", "最旧一条之后 Up 无变化");
        feed_seq(&mut ed, b"\x1b[B");
        assert_eq!(ed.line(), b"two");
        feed_seq(&mut ed, b"\x1b[B");
        assert_eq!(ed.line(), b"dr", "Down 回到浏览前的进行中行");
        feed_seq(&mut ed, b"\x1b[B");
        assert_eq!(ed.line(), b"dr", "已经在当前行，Down 无变化");
    }

    #[test]
    fn history_collapses_consecutive_duplicates() {
        let (mut ed, _) = new_editor();
        submit_text(&mut ed, "dup");
        submit_text(&mut ed, "dup");
        type_text(&mut ed, "cur");
        feed_seq(&mut ed, b"\x1b[A"); // "dup"（最近）
        feed_seq(&mut ed, b"\x1b[A"); // 折叠后只有一条：不动
        feed_seq(&mut ed, b"\x1b[B"); // 回到 None → 恢复 stash
        assert_eq!(ed.line(), b"cur", "重复历史折叠：Down 一步即回到暂存行");
    }

    #[test]
    fn empty_lines_are_not_stored_in_history() {
        let (mut ed, _) = new_editor();
        submit_text(&mut ed, "");
        type_text(&mut ed, "x");
        feed_seq(&mut ed, b"\x1b[A");
        assert_eq!(ed.line(), b"x", "空行不入历史，Up 应无效果");
    }

    #[test]
    fn empty_submit_then_browse_stays_at_stash() {
        let (mut ed, _) = new_editor();
        type_text(&mut ed, "keep");
        feed_seq(&mut ed, b"\x1b[A"); // 没有历史：无变化
        assert_eq!(ed.line(), b"keep");
        feed_byte(&mut ed, 0x15); // Ctrl-U：清掉进行中行，准备提交下一条
        assert_eq!(ed.line(), b"");
        submit_text(&mut ed, "kept");
        type_text(&mut ed, "next");
        feed_seq(&mut ed, b"\x1b[A");
        assert_eq!(ed.line(), b"kept");
        feed_seq(&mut ed, b"\x1b[B");
        assert_eq!(ed.line(), b"next");
    }

    // -- Tab 补全 ---------------------------------------------------------

    #[test]
    fn tab_unique_match_replaces_token_and_appends_space() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "he");
        assert_eq!(
            ed.feed("p> ", 0x09, &["help", "machine"], &mut screen),
            Outcome::Pending
        );
        assert_eq!(ed.line(), b"help ");
    }

    #[test]
    fn tab_multiple_matches_extend_to_common_prefix() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "lo");
        assert_eq!(
            ed.feed("p> ", 0x09, &["load", "loader"], &mut screen),
            Outcome::Pending
        );
        assert_eq!(ed.line(), b"load");
    }

    #[test]
    fn tab_multiple_matches_without_extension_print_list_then_redraw() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "m");
        assert_eq!(
            ed.feed("p> ", 0x09, &["machine", "memory"], &mut screen),
            Outcome::Pending
        );
        assert_eq!(ed.line(), b"m");
        let text = screen.bytes.clone();
        assert!(text.windows(7).any(|w| w == b"machine".as_slice()));
        assert!(text.windows(6).any(|w| w == b"memory".as_slice()));
        assert!(text.ends_with(b"m"), "候选列表后必须重绘当前行");
        assert!(!text.contains(&0x1b), "候选列表与重绘都不得含 ANSI");
    }

    #[test]
    fn tab_without_match_rings_bell() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "zz");
        assert_eq!(
            ed.feed("p> ", 0x09, &["help"], &mut screen),
            Outcome::Pending
        );
        assert_eq!(ed.line(), b"zz");
        assert!(screen.bytes.contains(&0x07));
    }

    #[test]
    fn tab_with_empty_candidates_rings_bell() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "he");
        assert_eq!(ed.feed("p> ", 0x09, &[], &mut screen), Outcome::Pending);
        assert_eq!(ed.line(), b"he");
        assert!(screen.bytes.contains(&0x07));
    }

    // -- 缓冲上限与控制字节 ----------------------------------------------

    #[test]
    fn buffer_full_rings_bell_without_inserting() {
        let (mut ed, mut screen) = new_editor();
        for _ in 0..LINE_MAX {
            assert_eq!(ed.feed("", b'a', &[], &mut screen), Outcome::Pending);
        }
        assert_eq!(ed.line().len(), LINE_MAX);

        screen.clear();
        assert_eq!(ed.feed("", b'b', &[], &mut screen), Outcome::Pending);
        assert_eq!(ed.line().len(), LINE_MAX, "满缓冲不得增长");
        assert!(ed.line().iter().all(|&b| b == b'a'));
        assert!(screen.bytes.contains(&0x07));
    }

    #[test]
    fn other_control_bytes_are_ignored() {
        let controls: &[u8] = &[
            0x00, 0x07, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x16, 0x18, 0x19, 0x1a, 0x1c,
            0x1d, 0x1e, 0x1f,
        ];
        for &byte in controls {
            let (mut ed, mut screen) = new_editor();
            assert_eq!(ed.feed("p> ", byte, &[], &mut screen), Outcome::Pending);
            assert!(ed.line().is_empty(), "控制字节不得插入缓冲");
            assert!(screen.bytes.is_empty(), "控制字节不得回显");
        }
    }

    // -- 行结束 -----------------------------------------------------------

    #[test]
    fn outcomes_submit_cancel_eof() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "hi");
        assert_eq!(ed.feed("p> ", b'\r', &[], &mut screen), Outcome::Submitted);
        assert_eq!(ed.line(), b"hi", "提交行在下一次 feed 前仍可读");

        type_text(&mut ed, "gone");
        screen.clear();
        assert_eq!(ed.feed("p> ", 0x03, &[], &mut screen), Outcome::Cancelled);
        assert!(ed.line().is_empty(), "Ctrl-C 丢弃缓冲");
        assert!(screen.bytes.windows(4).any(|w| w == b"^C\r\n".as_slice()));

        let (mut ed, mut screen) = new_editor();
        assert_eq!(ed.feed("p> ", 0x04, &[], &mut screen), Outcome::Eof);
        type_text(&mut ed, "x");
        assert_eq!(ed.feed("p> ", 0x04, &[], &mut screen), Outcome::Pending);
    }

    #[test]
    fn crlf_pair_produces_exactly_one_submit() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "ab");
        assert_eq!(ed.feed("", b'\r', &[], &mut screen), Outcome::Submitted);
        assert_eq!(ed.feed("", b'\n', &[], &mut screen), Outcome::Pending);
        assert!(ed.line().is_empty(), "CRLF 另一半之后缓冲已清理");

        // 单独 \n 也提交。
        type_text(&mut ed, "cd");
        assert_eq!(ed.feed("", b'\n', &[], &mut screen), Outcome::Submitted);
        assert_eq!(ed.line(), b"cd");
    }

    // -- 渲染字节流 -------------------------------------------------------

    #[test]
    fn redraw_stream_uses_only_cr_and_backspace() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "abc");
        screen.clear();

        feed_into(&mut ed, &mut screen, 0x02); // 左移 → 重绘
        feed_into(&mut ed, &mut screen, b'X'); // 插入 → 重绘
        feed_into(&mut ed, &mut screen, 0x7f); // 退格 → 重绘

        assert!(!screen.bytes.contains(&0x1b), "普通重绘不得含 ESC");
        assert!(screen.bytes.contains(&b'\r'));
        assert!(screen.bytes.contains(&0x08));
    }

    #[test]
    fn redraw_reemits_prompt() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "ab");
        screen.clear();
        feed_into(&mut ed, &mut screen, 0x02);
        assert!(screen.bytes.starts_with(b"\rp> "));
    }

    #[test]
    fn ctrl_l_clears_screen_then_redraws() {
        let (mut ed, mut screen) = new_editor();
        type_text(&mut ed, "abc");
        screen.clear();
        assert_eq!(ed.feed("p> ", 0x0c, &[], &mut screen), Outcome::Pending);
        assert!(screen.bytes.starts_with(b"\x1b[2J\x1b[H"));
        assert!(screen.bytes.windows(6).any(|w| w == b"p> abc".as_slice()));
        assert_eq!(ed.line(), b"abc");
    }

    #[test]
    fn prompt_and_line_fit_eighty_columns() {
        let prompt_len = "core> ".len();
        assert!(prompt_len + LINE_MAX <= 80);
    }
}
