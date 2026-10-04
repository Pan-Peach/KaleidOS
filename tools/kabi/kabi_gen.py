#!/usr/bin/env python3
"""KABI — KaleidOS 单一来源 ABI 生成器。

`abi/*.toml` 是 ABI 的单一来源（普通 TOML，无自定义 DSL）：枚举/数值、结构布局、
常量、extern 对象、fn 指针 typedef、extern 函数声明、函数表与符号名都写在 schema
里。本工具把它们渲染成各 target 的"哑 ABI"源码；人体工学封装、语义文档与 Core
实现仍然手写。

框架能力（跨 phase 复用）：

* **schema loader** —— 解析 + 严格校验（未知键 / 未知类型 / 悬空引用都报错）；
* **递归类型语法** —— ``void`` / ``u8``…``i64`` / ``usize`` / ``bool`` / ``char``
  （C ``char`` ↔ Rust ``u8``；只用于 ``*const char`` 这类 pointee）/ ``*const T`` /
  ``*mut T`` / 命名类型引用 / ``fn(...) -> ...``（unsafe extern "C" fn 指针）/
  ``safe fn(...) -> ...``（safe extern "C" fn 指针）；fn 参数可带名字
  （``fn(ctx: *mut void) -> void``），C / Rust 声明里都渲染出来，
  解析成类型树后**按语言渲染**，不对类型名做字符串替换；
* **per-target emitters** —— target = C / SDK-Rust / Core-Rust / Core-export-table，
  target 特有行为只在 emitter 里，不在 schema 里；
* **多 schema 输出** —— 一个输出文件声明它的输入 schema 列表，按列表顺序拼接；
* **check mode** —— 重生成到临时目录并与提交物逐文件 diff：内容漂移、生成文件缺失、
  生成目录出现计划外文件，三者都会失败。

声明形状（`[[struct]]` 等）由 schema 决定，emitter 只做机械渲染：

* ``[[struct]]`` 可带 ``size`` / ``size64``+``size32`` / ``align``（字面布局断言），
  也可带 ``size_ptrs = N``——"N 个指针宽字段"的结构（函数表 / 扁平 frame）：字段
  必须是指针、fn 指针或 ``usize``；C 断言
  ``sizeof(struct X) == N * sizeof(void *)``，Rust 断言
  ``size_of::<X>() == N * size_of::<usize>()``（外加指针对齐），
  字段个数 / 顺序漂移 = 编译错误；
* ``[[const]]`` 类型可以是整数 primitive，也可以是 ``string``（C
  ``#define NAME "..."`` / Rust ``&[u8] = b"..."``）。

用法（仓库根目录）：

    python3 tools/kabi/kabi_gen.py generate --schema abi/errno.toml --schema abi/core.toml \\
        --schema abi/component.toml --out-root .
    python3 tools/kabi/kabi_gen.py check    --schema ... --out-root .
    python3 tools/kabi/kabi_gen.py selftest

仅标准库。`tomllib` 是 Python 3.11+ 才有，而本仓库的 `python3` 可能更旧，因此带一个
覆盖 schema 子集的 TOML 回退解析器；两条路径对同一 schema 必须得到同一份数据
（文档尾随空行会被归一化，保证生成确定 —— 无时间戳、无绝对路径、无宿主信息）。
"""

from __future__ import annotations

import argparse
import difflib
import os
import posixpath
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Set, Tuple, Union

GENERATOR = "tools/kabi/kabi_gen.py"
SCHEMA_DIR = "abi"

# 声明级 target 过滤：一个声明只在列出的 target 里出现。
DECL_TARGETS = ("c", "sdk-rust", "core-rust")


class KabiError(Exception):
    """Schema / 生成过程中的用户可见错误（不打印 traceback）。"""


# ===========================================================================
# 类型语法：字符串 → 类型树 → 每语言渲染
# ===========================================================================

C_PRIMITIVES = {
    "void": "void",
    "u8": "uint8_t",
    "u16": "uint16_t",
    "u32": "uint32_t",
    "u64": "uint64_t",
    "i8": "int8_t",
    "i16": "int16_t",
    "i32": "int32_t",
    "i64": "int64_t",
    "usize": "size_t",
    "bool": "bool",
    # C 侧是 `char`（不是 `uint8_t`）：FatFs 之类接口的 `const char *path` 必须
    # 逐字保留；Rust 侧同一契约是 `u8`（`*const u8`）。只作为指针 pointee 使用。
    "char": "char",
}
RUST_PRIMITIVES = {
    "void": "()",
    "u8": "u8",
    "u16": "u16",
    "u32": "u32",
    "u64": "u64",
    "i8": "i8",
    "i16": "i16",
    "i32": "i32",
    "i64": "i64",
    "usize": "usize",
    "bool": "bool",
    "char": "u8",
}
PRIMITIVES = set(C_PRIMITIVES)
# `char` 不参与整数常量（没有字符字面量语法）；字符串常量走 `Str`。
INTEGER_PRIMITIVES = PRIMITIVES - {"void", "bool", "char"}


@dataclass(frozen=True)
class Prim:
    name: str


@dataclass(frozen=True)
class Ptr:
    mutable: bool
    inner: "TypeNode"


@dataclass(frozen=True)
class Named:
    """命名类型引用（schema 里声明的 struct / enum / alias 名）。"""

    name: str


@dataclass(frozen=True)
class Fn:
    """fn 指针类型。``safe`` = Rust ``extern "C" fn``（否则 ``unsafe extern "C" fn``）。

    ``names`` 与 ``params`` 等长（缺省 = 全部匿名）：C / Rust 声明都会渲染出
    参数名，便于人读；参数名不是类型的一部分。
    """

    params: Tuple["TypeNode", ...]
    ret: "TypeNode"
    safe: bool = False
    names: Tuple[Optional[str], ...] = ()


@dataclass(frozen=True)
class Str:
    """字符串常量类型（**只用于** ``[[const]]``）：C `#define NAME "..."` /
    Rust ``&[u8] = b"..."``。"""

    name: str = "string"


TypeNode = Union[Prim, Ptr, Named, Fn, Str]

_IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_TOKEN_RE = re.compile(r"\s*(\*|->|[(),:]|[A-Za-z_][A-Za-z0-9_]*)")


def _tokenize_type(text: str) -> List[str]:
    tokens: List[str] = []
    pos = 0
    while pos < len(text):
        match = _TOKEN_RE.match(text, pos)
        if match is None:
            rest = text[pos:].strip()
            if not rest:
                break
            raise KabiError("invalid type %r: unexpected character at %d" % (text, pos))
        tokens.append(match.group(1))
        pos = match.end()
    return tokens


class _TypeParser:
    def __init__(self, tokens: List[str], text: str, known: Set[str]) -> None:
        self._tokens = tokens
        self._text = text
        self._known = known
        self._pos = 0

    def _peek(self) -> Optional[str]:
        return self._tokens[self._pos] if self._pos < len(self._tokens) else None

    def _peek2(self) -> Optional[str]:
        return self._tokens[self._pos + 1] if self._pos + 1 < len(self._tokens) else None

    def _next(self) -> Optional[str]:
        token = self._peek()
        self._pos += 1
        return token

    def _take_param_name(self) -> Optional[str]:
        """fn 参数的可选 `name:` 前缀（参数名不是类型的一部分，只为可读性）。"""
        token = self._peek()
        if token is not None and _IDENT_RE.fullmatch(token) and self._peek2() == ":":
            self._next()
            self._next()
            return token
        return None

    def _expect(self, expected: str) -> None:
        token = self._next()
        if token != expected:
            raise KabiError("invalid type %r: expected %r, got %r" % (self._text, expected, token))

    def parse(self) -> TypeNode:
        node = self._type()
        if self._pos != len(self._tokens):
            raise KabiError("invalid type %r: trailing %r" % (self._text, self._tokens[self._pos:]))
        return node

    def _type(self) -> TypeNode:
        token = self._peek()
        if token is None:
            raise KabiError("invalid type %r: unexpected end" % self._text)
        if token == "*":
            self._next()
            qualifier = self._next()
            if qualifier not in ("const", "mut"):
                raise KabiError(
                    "invalid type %r: expected 'const' or 'mut' after '*'" % self._text
                )
            return Ptr(mutable=(qualifier == "mut"), inner=self._type())
        safe = False
        if token == "safe":
            self._next()
            safe = True
            token = self._peek()
            if token != "fn":
                raise KabiError("invalid type %r: expected 'fn' after 'safe'" % self._text)
        if token == "fn":
            self._next()
            self._expect("(")
            params: List[TypeNode] = []
            names: List[Optional[str]] = []
            if self._peek() != ")":
                while True:
                    names.append(self._take_param_name())
                    params.append(self._type())
                    if self._peek() != ",":
                        break
                    self._next()
            self._expect(")")
            self._expect("->")
            if all(name is None for name in names):
                names = []  # 全匿名 = 规范化成缺省表示（与手写 Fn 字面量可比）
            return Fn(params=tuple(params), ret=self._type(), safe=safe, names=tuple(names))
        if not _IDENT_RE.fullmatch(token):
            raise KabiError("invalid type %r: unexpected token %r" % (self._text, token))
        self._next()
        if token in PRIMITIVES:
            return Prim(token)
        if token in self._known:
            return Named(token)
        raise KabiError("unknown type %r in %r" % (token, self._text))


def parse_type(text: str, known: Sequence[str] = (), allow_void: bool = False) -> TypeNode:
    """把 schema 的类型字符串解析成类型树；未知类型 / 非法 void 位置直接报错。

    `allow_void` 只给"函数返回类型"用：`void` 允许作为裸返回类型。
    """
    node = _TypeParser(_tokenize_type(text), text, set(known)).parse()
    _check_void_positions(node, text, allow_void)
    return node


def _check_void_positions(node: TypeNode, where: str, allow_void: bool = False) -> None:
    """`void` 只允许出现在函数返回类型或指针的直接 pointee（`*mut void`）。"""
    if isinstance(node, Prim):
        if node.name == "void" and not allow_void:
            raise KabiError(
                "invalid void position in type %r: use it as a return type or behind a pointer"
                % where
            )
        return
    if isinstance(node, Ptr):
        if isinstance(node.inner, Prim) and node.inner.name == "void":
            return
        _check_void_positions(node.inner, where)
        return
    if isinstance(node, Fn):
        for param in node.params:
            _check_void_positions(param, where)
        if not (isinstance(node.ret, Prim) and node.ret.name == "void"):
            _check_void_positions(node.ret, where)


# --- 类型级渲染（无上下文；`render_type` 保留给独立使用与自测）-----------------


def render_type(node: TypeNode, language: str) -> str:
    if language == "c":
        return _render_c(node)
    if language == "rust":
        return _render_rust(node)
    raise KabiError("unknown language %r (expected 'c' or 'rust')" % language)


def _fn_params(node: Fn) -> Tuple[Tuple[TypeNode, Optional[str]], ...]:
    """`(参数类型, 可选参数名)` 序列；schema 里参数名可以全省略。"""
    if not node.names:
        return tuple((param, None) for param in node.params)
    if len(node.names) != len(node.params):
        raise KabiError("fn type param name count mismatch")
    return tuple(zip(node.params, node.names))


def _contains_fn_pointer(node: TypeNode) -> bool:
    """字段类型里（含指针 pointee）是否有函数指针。

    Rust 对函数指针的 `==` 会触发 `unpredictable_function_pointer_comparisons`
    （地址不保证唯一），因此含 fn 指针的 struct **不**派生 `PartialEq` / `Eq`。
    """
    if isinstance(node, Fn):
        return True
    if isinstance(node, Ptr):
        return _contains_fn_pointer(node.inner)
    return False


def _render_c(node: TypeNode) -> str:
    if isinstance(node, Prim):
        return C_PRIMITIVES[node.name]
    if isinstance(node, Named):
        return "struct %s" % node.name
    if isinstance(node, Fn):
        return _render_c_fn(node)
    if isinstance(node, Ptr):
        if isinstance(node.inner, Fn):
            return _render_c_fn(node.inner)
        inner = _render_c(node.inner)
        stars = "*" if inner.endswith("*") else " *"
        return ("const " if not node.mutable else "") + inner + stars
    raise KabiError("unrenderable type node %r" % (node,))


def _render_c_fn(node: Fn, names: Optional[Dict[str, str]] = None) -> str:
    names = names or {}
    params = (
        ", ".join(c_decl(param, name or "", names) for param, name in _fn_params(node)) or "void"
    )
    return "%s (*)(%s)" % (_render_c(node.ret), params)


def _render_rust(node: TypeNode) -> str:
    if isinstance(node, Prim):
        return RUST_PRIMITIVES[node.name]
    if isinstance(node, Named):
        return node.name
    if isinstance(node, Ptr):
        return ("*const " if not node.mutable else "*mut ") + _render_rust(node.inner)
    if isinstance(node, Fn):
        params = ", ".join(
            ("%s: %s" % (name, _render_rust(param))) if name else _render_rust(param)
            for param, name in _fn_params(node)
        )
        head = 'extern "C" fn' if node.safe else 'unsafe extern "C" fn'
        rendered = "%s(%s)" % (head, params)
        ret = _render_rust(node.ret)
        return rendered if ret == "()" else rendered + " -> " + ret
    raise KabiError("unrenderable type node %r" % (node,))


# --- C 声明渲染（带名字的 declarator；命名类型需要 schema 上下文）---------------


def c_base(node: TypeNode, names: Dict[str, str]) -> str:
    if isinstance(node, Prim):
        return C_PRIMITIVES[node.name]
    if isinstance(node, Named):
        try:
            return names[node.name]
        except KeyError:
            raise KabiError("no C spelling registered for type %r" % node.name)
    raise KabiError("type %r has no C base spelling" % (node,))


def c_decl(node: TypeNode, decl: str, names: Dict[str, str], const: bool = False) -> str:
    """渲染 `decl`（如 `x` / `*x` / `**x`）的 C 声明，返回 `T decl` 形式。"""
    if isinstance(node, Ptr):
        return c_decl(node.inner, "*" + decl, names, const=not node.mutable)
    if isinstance(node, Fn):
        params = (
            ", ".join(c_decl(param, name or "", names) for param, name in _fn_params(node)) or "void"
        )
        return c_decl(node.ret, "(*%s)(%s)" % (decl, params), names)
    return ("const " if const else "") + c_base(node, names) + " " + decl


def c_fn_decl(ret: TypeNode, params: Sequence["Param"], name: str, names: Dict[str, str]) -> str:
    args = ", ".join(c_decl(param.type, param.name, names) for param in params) or "void"
    return c_decl(ret, "%s(%s)" % (name, args), names)


def c_fn_typedef(ret: TypeNode, params: Sequence["Param"], name: str, names: Dict[str, str]) -> str:
    args = ", ".join(c_decl(param.type, param.name, names) for param in params) or "void"
    return "typedef " + c_decl(ret, "(*%s)(%s)" % (name, args), names) + ";"


def rust_fn_type(
    ret: TypeNode, params: Sequence["Param"], safe: bool, with_names: bool
) -> str:
    head = 'extern "C" fn' if safe else 'unsafe extern "C" fn'
    if with_names:
        args = ", ".join("%s: %s" % (param.name, _render_rust(param.type)) for param in params)
    else:
        args = ", ".join(_render_rust(param.type) for param in params)
    rendered = "%s(%s)" % (head, args)
    ret_rendered = _render_rust(ret)
    return rendered if ret_rendered == "()" else rendered + " -> " + ret_rendered


# ===========================================================================
# TOML：stdlib tomllib（3.11+）或 schema 子集回退解析器
# ===========================================================================


def _read_text(path: str) -> str:
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read()


def _load_toml(path: str) -> dict:
    try:
        import tomllib  # Python 3.11+
    except ImportError:
        try:
            return _parse_toml_subset(_read_text(path))
        except KabiError as exc:
            raise KabiError("%s: %s" % (path, exc))
    with open(path, "rb") as handle:
        try:
            return tomllib.load(handle)
        except tomllib.TOMLDecodeError as exc:
            raise KabiError("%s: invalid TOML: %s" % (path, exc))


def _parse_toml_subset(text: str) -> dict:
    """schema 用到的 TOML 子集：注释 / 表头 / 数组表 / 字符串（含 `\"\"\"`）/ 整数 / 布尔。

    不支持数组、内联表、日期、dotted keys —— schema 语法不允许使用它们。
    """
    root: dict = {}
    current: dict = root
    lines = text.split("\n")
    index = 0
    while index < len(lines):
        line = lines[index].strip()
        index += 1
        if not line or line.startswith("#"):
            continue
        if line.startswith("[["):
            match = re.fullmatch(r"\[\[\s*([A-Za-z0-9_.]+)\s*\]\]", line)
            if match is None:
                raise KabiError("invalid table header %r" % line)
            current = _append_table(root, match.group(1).split("."))
            continue
        if line.startswith("["):
            match = re.fullmatch(r"\[\s*([A-Za-z0-9_.]+)\s*\]", line)
            if match is None:
                raise KabiError("invalid table header %r" % line)
            current = _descend_table(root, match.group(1).split("."))
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)", line)
        if match is None:
            raise KabiError("invalid TOML line %r" % line)
        value, index = _parse_toml_value(match.group(2).strip(), lines, index)
        if match.group(1) in current:
            raise KabiError("duplicate key %r" % match.group(1))
        current[match.group(1)] = value
    return root


def _append_table(root: dict, parts: List[str]) -> dict:
    node = root
    for key in parts[:-1]:
        node = _child_table(node, key)
    existing = node.get(parts[-1])
    if existing is None:
        existing = []
        node[parts[-1]] = existing
    if not isinstance(existing, list):
        raise KabiError("table %r already declared as a non-array" % ".".join(parts))
    table: dict = {}
    existing.append(table)
    return table


def _descend_table(root: dict, parts: List[str]) -> dict:
    node = root
    for key in parts:
        node = _child_table(node, key)
    return node


def _child_table(node: dict, key: str) -> dict:
    value = node.get(key)
    if value is None:
        value = {}
        node[key] = value
    if isinstance(value, list):
        if not value:
            raise KabiError("array table %r has no element" % key)
        value = value[-1]
    if not isinstance(value, dict):
        raise KabiError("key %r is not a table" % key)
    return value


def _parse_toml_value(rest: str, lines: List[str], index: int):
    if rest.startswith('"""'):
        body = rest[3:]
        first = True
        while '"""' not in body:
            if index >= len(lines):
                raise KabiError("unterminated multi-line string")
            if first and not body:
                body = lines[index]
            else:
                body += "\n" + lines[index]
            first = False
            index += 1
        return body[: body.index('"""')], index
    if rest.startswith('"'):
        match = re.fullmatch(r'"((?:[^"\\]|\\.)*)"\s*', rest)
        if match is None:
            raise KabiError("invalid string value %r" % rest)
        return _unescape(match.group(1)), index
    if rest in ("true", "false"):
        return rest == "true", index
    if re.fullmatch(r"[+-]?[0-9](_?[0-9])*", rest):
        return int(rest.replace("_", "")), index
    raise KabiError("unsupported value %r (schema only uses strings / ints / bools)" % rest)


def _unescape(text: str) -> str:
    escapes = {'"': '"', "\\": "\\", "n": "\n", "t": "\t", "r": "\r"}
    out: List[str] = []
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\":
            index += 1
            if index >= len(text) or text[index] not in escapes:
                raise KabiError("unsupported escape in string %r" % text)
            out.append(escapes[text[index]])
        else:
            out.append(char)
        index += 1
    return "".join(out)


# ===========================================================================
# Schema 模型
# ===========================================================================

RUST_REPR_RANGES = {
    "i8": (-(2 ** 7), 2 ** 7 - 1),
    "i16": (-(2 ** 15), 2 ** 15 - 1),
    "i32": (-(2 ** 31), 2 ** 31 - 1),
    "i64": (-(2 ** 63), 2 ** 63 - 1),
    "u8": (0, 2 ** 8 - 1),
    "u16": (0, 2 ** 16 - 1),
    "u32": (0, 2 ** 32 - 1),
    "u64": (0, 2 ** 64 - 1),
    "usize": (0, 2 ** 64 - 1),
}
C_STYLES = ("defines", "enum")


@dataclass(frozen=True)
class Param:
    name: str
    type: TypeNode
    doc: str = ""


@dataclass(frozen=True)
class Variant:
    name: str
    c_name: str
    value: int
    doc: str = ""


@dataclass(frozen=True)
class Enum:
    name: str
    c_name: str
    doc: str
    repr: str
    c_style: str
    variants: Tuple[Variant, ...]
    targets: Tuple[str, ...] = DECL_TARGETS
    decode_fallback: Optional[str] = None


@dataclass(frozen=True)
class StructField:
    name: str
    type: TypeNode
    doc: str = ""
    offset64: Optional[int] = None
    offset32: Optional[int] = None


@dataclass(frozen=True)
class Struct:
    name: str
    c_name: str
    doc: str
    fields: Tuple[StructField, ...]
    size64: Optional[int] = None
    size32: Optional[int] = None
    align: Optional[int] = None
    # 函数表：字段必须全是指针宽，布局断言 = N * 指针宽（C `sizeof(void *)` /
    # Rust `size_of::<usize>()`）+ 指针对齐。与 size / size64 / size32 / align 互斥。
    size_ptrs: Optional[int] = None
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Alias:
    """命名 fn 指针类型（C typedef / Rust type alias）。"""

    name: str
    c_name: str
    doc: str
    ret: TypeNode
    params: Tuple[Param, ...]
    safe: bool
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Entry:
    """组件**导出**的生命周期入口：C 是函数声明，SDK-Rust 是类型别名。"""

    name: str
    alias: str
    doc: str
    ret: TypeNode
    params: Tuple[Param, ...]
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Object:
    """外部对象符号（如 `extern const uint64_t kcomp_abi;`）。"""

    name: str
    type: TypeNode
    is_const: bool
    doc: str
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Constant:
    name: str
    type: TypeNode
    literal: str
    value: Optional[int]
    doc: str
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Function:
    """kcore_* 导出：C/SDK 声明 + Core 实现签名（可显式覆盖）+ 导出表条目。"""

    name: str
    ret: TypeNode
    params: Tuple[Param, ...]
    core_params: Tuple[Param, ...]
    doc: str
    c_doc: str
    category: str
    c_category: str
    category_group: str
    targets: Tuple[str, ...] = DECL_TARGETS


@dataclass(frozen=True)
class Schema:
    path: str
    name: str
    banner: str
    enums: Tuple[Enum, ...] = field(default_factory=tuple)
    structs: Tuple[Struct, ...] = field(default_factory=tuple)
    aliases: Tuple[Alias, ...] = field(default_factory=tuple)
    entries: Tuple[Entry, ...] = field(default_factory=tuple)
    objects: Tuple[Object, ...] = field(default_factory=tuple)
    constants: Tuple[Constant, ...] = field(default_factory=tuple)
    functions: Tuple[Function, ...] = field(default_factory=tuple)


def _normalize_doc(text: str) -> str:
    """归一化 schema 文档：行尾空白 / 首尾空行剥离（tomllib 与回退解析器一致）。"""
    lines = [line.rstrip() for line in text.split("\n")]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines)


def _reject_unknown(table: dict, allowed: Sequence[str], where: str, path: str) -> None:
    unknown = sorted(set(table) - set(allowed))
    if unknown:
        raise KabiError("%s: unknown key(s) in %s: %s" % (path, where, ", ".join(unknown)))


def _required_string(table: dict, key: str, where: str, path: str) -> str:
    value = table.get(key)
    if not isinstance(value, str):
        raise KabiError("%s: %s needs a string %r" % (path, where, key))
    return value


def _required_int(table: dict, key: str, where: str, path: str) -> int:
    value = table.get(key)
    if isinstance(value, bool) or not isinstance(value, int):
        raise KabiError("%s: %s needs an integer %r" % (path, where, key))
    return value


def _table_list(table: dict, key: str, path: str) -> List[dict]:
    value = table.get(key)
    if value is None:
        return []
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise KabiError("%s: %r must be declared as [[%s]] tables" % (path, key, key))
    return value


def _doc_key(table: dict, key: str, path: str, where: str) -> str:
    value = table.get(key, "")
    if not isinstance(value, str):
        raise KabiError("%s: %s %s must be a string" % (path, where, key))
    return _normalize_doc(value)


def _doc(table: dict, path: str, where: str) -> str:
    return _doc_key(table, "doc", path, where)


def _targets(table: dict, path: str, where: str) -> Tuple[str, ...]:
    raw = table.get("targets")
    if raw is None:
        return DECL_TARGETS
    if not isinstance(raw, str):
        raise KabiError("%s: %s targets must be a comma-separated string" % (path, where))
    names = tuple(part.strip() for part in raw.split(",") if part.strip())
    unknown = [name for name in names if name not in DECL_TARGETS]
    if unknown:
        raise KabiError("%s: %s has unknown target(s): %s" % (path, where, ", ".join(unknown)))
    if not names:
        raise KabiError("%s: %s targets must not be empty" % (path, where))
    return names


def load_schema(path: str) -> Schema:
    """单 schema 便捷入口（自测 / 独立使用）。跨 schema 引用用 [`load_schemas`]。"""
    return load_schemas([path])[0]


def load_schemas(paths: Sequence[str]) -> List[Schema]:
    """两阶段加载：先收集所有 schema 声明的类型名，再构建（允许跨 schema 引用）。"""
    pending = []
    seen: Dict[str, str] = {}
    for path in paths:
        banner = _schema_banner(path)
        data = _load_toml(path)
        declared = _declared_type_names(data, path)
        for name in declared:
            if name in seen:
                raise KabiError(
                    "%s: type %r already declared by %s (cross-schema duplicate)"
                    % (banner, name, seen[name])
                )
            seen[name] = banner
        pending.append((path, banner, data))
    known = set(seen)
    return [_build_schema(data, path, banner, known) for path, banner, data in pending]


def _declared_type_names(data: dict, path: str) -> List[str]:
    names: List[str] = []
    for kind in ("enum", "struct", "alias"):
        for table in _table_list(data, kind, path):
            name = table.get("name")
            if not isinstance(name, str) or not _IDENT_RE.fullmatch(name):
                raise KabiError("%s: every [[%s]] needs a valid `name`" % (path, kind))
            names.append(name)
    if len(set(names)) != len(names):
        raise KabiError("%s: duplicate type name within schema" % path)
    return names


def _schema_banner(path: str) -> str:
    if not path.endswith(".toml"):
        raise KabiError("schema %s must be a .toml file" % path)
    directory = os.path.basename(os.path.normpath(os.path.dirname(path) or "."))
    if directory != SCHEMA_DIR:
        raise KabiError("schema %s must live in %s/ (banner records its repo path)" % (path, SCHEMA_DIR))
    return "%s/%s" % (SCHEMA_DIR, os.path.basename(path))


def _build_schema(data: dict, path: str, banner: str, known: Optional[Set[str]] = None) -> Schema:
    allowed = ("enum", "struct", "alias", "entry", "object", "const", "function")
    _reject_unknown(data, allowed, "schema", path)
    enum_tables = _table_list(data, "enum", path)
    struct_tables = _table_list(data, "struct", path)
    alias_tables = _table_list(data, "alias", path)
    entry_tables = _table_list(data, "entry", path)
    object_tables = _table_list(data, "object", path)
    const_tables = _table_list(data, "const", path)
    function_tables = _table_list(data, "function", path)
    if known is None:
        known = set(_declared_type_names(data, path))
    enums = tuple(_build_enum(table, path, known) for table in enum_tables)
    structs = tuple(_build_struct(table, path, known) for table in struct_tables)
    aliases = tuple(_build_alias(table, path, known) for table in alias_tables)
    entries = tuple(_build_entry(table, path, known) for table in entry_tables)
    objects = tuple(_build_object(table, path, known) for table in object_tables)
    constants = tuple(_build_constant(table, path, known) for table in const_tables)
    functions = tuple(_build_function(table, path, known) for table in function_tables)
    for kind, names in (
        ("entry", [entry.name for entry in entries]),
        ("object", [obj.name for obj in objects]),
        ("const", [const.name for const in constants]),
        ("function", [func.name for func in functions]),
    ):
        if len(set(names)) != len(names):
            raise KabiError("%s: duplicate [[%s]] names" % (path, kind))
    if not (enums or structs or aliases or entries or objects or constants or functions):
        raise KabiError("%s: schema declares nothing" % path)
    name = os.path.basename(path)[: -len(".toml")]
    return Schema(
        path=path,
        name=name,
        banner=banner,
        enums=enums,
        structs=structs,
        aliases=aliases,
        entries=entries,
        objects=objects,
        constants=constants,
        functions=functions,
    )


def _build_params(
    table: dict, key: str, path: str, where: str, known: Set[str]
) -> Tuple[Param, ...]:
    params: List[Param] = []
    for param_table in _table_list(table, key, path):
        _reject_unknown(param_table, ("name", "type", "doc"), "[[%s]] of %s" % (key, where), path)
        name = _required_string(param_table, "name", "%s param" % where, path)
        if not _IDENT_RE.fullmatch(name):
            raise KabiError("%s: %s param name %r is not an identifier" % (path, where, name))
        type_text = _required_string(param_table, "type", "%s param %s" % (where, name), path)
        try:
            node = parse_type(type_text, sorted(known))
        except KabiError as exc:
            raise KabiError("%s: %s param %s: %s" % (path, where, name, exc))
        params.append(Param(name, node, _doc(param_table, path, "%s param %s" % (where, name))))
    names = [param.name for param in params]
    if len(set(names)) != len(names):
        raise KabiError("%s: %s has duplicate param names" % (path, where))
    return tuple(params)


def _build_enum(table: dict, path: str, known: Set[str]) -> Enum:
    _reject_unknown(
        table,
        ("name", "c_name", "doc", "repr", "c_style", "targets", "decode_fallback", "variant"),
        "[[enum]] %r" % table.get("name"),
        path,
    )
    name = table["name"]
    c_name = table.get("c_name", name)
    if not isinstance(c_name, str) or not _IDENT_RE.fullmatch(c_name):
        raise KabiError("%s: enum %s c_name must be an identifier" % (path, name))
    repr_ = _required_string(table, "repr", "enum %s" % name, path)
    if repr_ not in RUST_REPR_RANGES:
        raise KabiError("%s: enum %s has unsupported repr %r" % (path, name, repr_))
    c_style = _required_string(table, "c_style", "enum %s" % name, path)
    if c_style not in C_STYLES:
        raise KabiError("%s: enum %s has unsupported c_style %r" % (path, name, c_style))
    variant_tables = _table_list(table, "variant", path)
    if not variant_tables:
        raise KabiError("%s: enum %s declares no [[enum.variant]]" % (path, name))
    low, high = RUST_REPR_RANGES[repr_]
    variants: List[Variant] = []
    for variant_table in variant_tables:
        _reject_unknown(
            variant_table, ("name", "c_name", "value", "doc"), "[[enum.variant]] of %s" % name, path
        )
        variant_name = _required_string(variant_table, "name", "variant of %s" % name, path)
        if not _IDENT_RE.fullmatch(variant_name):
            raise KabiError("%s: enum %s variant %r is not an identifier" % (path, name, variant_name))
        variant_c_name = variant_table.get("c_name", variant_name)
        if not isinstance(variant_c_name, str) or not _IDENT_RE.fullmatch(variant_c_name):
            raise KabiError(
                "%s: enum %s variant %s c_name must be an identifier" % (path, name, variant_name)
            )
        value = variant_table.get("value")
        if isinstance(value, bool) or not isinstance(value, int):
            raise KabiError("%s: enum %s variant %s needs an integer value" % (path, name, variant_name))
        if not low <= value <= high:
            raise KabiError(
                "%s: enum %s variant %s value %d out of %s range" % (path, name, variant_name, value, repr_)
            )
        variants.append(
            Variant(variant_name, variant_c_name, value, _doc(variant_table, path, variant_name))
        )
    names = [variant.name for variant in variants]
    if len(set(names)) != len(names):
        raise KabiError("%s: enum %s has duplicate variant names" % (path, name))
    values = [variant.value for variant in variants]
    if len(set(values)) != len(values):
        raise KabiError("%s: enum %s has duplicate variant values" % (path, name))
    fallback = table.get("decode_fallback")
    if fallback is not None and (not isinstance(fallback, str) or fallback not in set(names)):
        raise KabiError("%s: enum %s decode_fallback must name an existing variant" % (path, name))
    return Enum(
        name=name,
        c_name=c_name,
        doc=_doc(table, path, "enum %s" % name),
        repr=repr_,
        c_style=c_style,
        variants=tuple(variants),
        targets=_targets(table, path, "enum %s" % name),
        decode_fallback=fallback,
    )


def _build_struct(table: dict, path: str, known: Set[str]) -> Struct:
    _reject_unknown(
        table,
        (
            "name",
            "c_name",
            "doc",
            "size",
            "size64",
            "size32",
            "align",
            "size_ptrs",
            "targets",
            "field",
        ),
        "[[struct]] %r" % table.get("name"),
        path,
    )
    name = table["name"]
    c_name = table.get("c_name", name)
    if not isinstance(c_name, str) or not _IDENT_RE.fullmatch(c_name):
        raise KabiError("%s: struct %s c_name must be an identifier" % (path, name))
    fields: List[StructField] = []
    for field_table in _table_list(table, "field", path):
        _reject_unknown(
            field_table,
            ("name", "type", "doc", "offset", "offset64", "offset32"),
            "[[struct.field]] of %s" % name,
            path,
        )
        field_name = _required_string(field_table, "name", "field of %s" % name, path)
        if not _IDENT_RE.fullmatch(field_name):
            raise KabiError("%s: struct %s field %r is not an identifier" % (path, name, field_name))
        type_text = _required_string(field_table, "type", "field %s.%s" % (name, field_name), path)
        try:
            node = parse_type(type_text, sorted(known))
        except KabiError as exc:
            raise KabiError("%s: struct %s field %s: %s" % (path, name, field_name, exc))
        if "offset" in field_table and ("offset64" in field_table or "offset32" in field_table):
            raise KabiError(
                "%s: struct %s field %s must use either offset or offset64/offset32"
                % (path, name, field_name)
            )
        offset = field_table.get("offset")
        offset64 = field_table.get("offset64")
        offset32 = field_table.get("offset32")
        if offset is not None:
            offset64 = offset32 = _required_int(
                field_table, "offset", "field %s.%s" % (name, field_name), path
            )
        if (offset64 is None) != (offset32 is None):
            raise KabiError(
                "%s: struct %s field %s needs both offset64 and offset32" % (path, name, field_name)
            )
        fields.append(
            StructField(
                field_name,
                node,
                _doc(field_table, path, "field %s.%s" % (name, field_name)),
                offset64,
                offset32,
            )
        )
    field_names = [item.name for item in fields]
    if len(set(field_names)) != len(field_names):
        raise KabiError("%s: struct %s has duplicate field names" % (path, name))
    size = table.get("size")
    size64 = table.get("size64")
    size32 = table.get("size32")
    if size is not None:
        size64 = size32 = _required_int(table, "size", "struct %s" % name, path)
    if (size64 is None) != (size32 is None):
        raise KabiError("%s: struct %s needs both size64 and size32" % (path, name))
    align = table.get("align")
    if align is not None:
        align = _required_int(table, "align", "struct %s" % name, path)
    size_ptrs = table.get("size_ptrs")
    if size_ptrs is not None:
        if isinstance(size_ptrs, bool) or not isinstance(size_ptrs, int) or size_ptrs < 1:
            raise KabiError("%s: struct %s size_ptrs must be a positive integer" % (path, name))
        if size is not None or size64 is not None or align is not None:
            raise KabiError(
                "%s: struct %s must not combine size_ptrs with size / size64 / size32 / align"
                % (path, name)
            )
        for field in fields:
            # 指针宽 = 指针 / fn 指针 / `usize`（`size_t`）。`usize` 是"语义就是
            # 指针宽"的长度量（AGENTS.md 宽度规则），与 `*const void` 同宽。
            if isinstance(field.type, Prim) and field.type.name == "usize":
                continue
            if not isinstance(field.type, (Ptr, Fn)):
                raise KabiError(
                    "%s: struct %s size_ptrs requires pointer-sized fields (%s is not)"
                    % (path, name, field.name)
                )
    return Struct(
        name=name,
        c_name=c_name,
        doc=_doc(table, path, "struct %s" % name),
        fields=tuple(fields),
        size64=size64,
        size32=size32,
        align=align,
        size_ptrs=size_ptrs,
        targets=_targets(table, path, "struct %s" % name),
    )


def _build_alias(table: dict, path: str, known: Set[str]) -> Alias:
    _reject_unknown(
        table,
        ("name", "c_name", "doc", "ret", "param", "safe", "targets"),
        "[[alias]] %r" % table.get("name"),
        path,
    )
    name = table["name"]
    c_name = table.get("c_name", name)
    if not isinstance(c_name, str) or not _IDENT_RE.fullmatch(c_name):
        raise KabiError("%s: alias %s c_name must be an identifier" % (path, name))
    ret = parse_type(
        _required_string(table, "ret", "alias %s" % name, path), sorted(known), allow_void=True
    )
    params = _build_params(table, "param", path, "alias %s" % name, known)
    safe = table.get("safe", True)
    if not isinstance(safe, bool):
        raise KabiError("%s: alias %s safe must be a bool" % (path, name))
    return Alias(
        name=name,
        c_name=c_name,
        doc=_doc(table, path, "alias %s" % name),
        ret=ret,
        params=params,
        safe=safe,
        targets=_targets(table, path, "alias %s" % name),
    )


def _build_entry(table: dict, path: str, known: Set[str]) -> Entry:
    _reject_unknown(
        table,
        ("name", "alias", "doc", "ret", "param", "targets"),
        "[[entry]] %r" % table.get("name"),
        path,
    )
    name = _required_string(table, "name", "entry", path)
    alias = _required_string(table, "alias", "entry %s" % name, path)
    if not _IDENT_RE.fullmatch(alias):
        raise KabiError("%s: entry %s alias %r is not an identifier" % (path, name, alias))
    ret = parse_type(
        _required_string(table, "ret", "entry %s" % name, path), sorted(known), allow_void=True
    )
    params = _build_params(table, "param", path, "entry %s" % name, known)
    return Entry(
        name=name,
        alias=alias,
        doc=_doc(table, path, "entry %s" % name),
        ret=ret,
        params=params,
        targets=_targets(table, path, "entry %s" % name),
    )


def _build_object(table: dict, path: str, known: Set[str]) -> Object:
    _reject_unknown(
        table, ("name", "type", "const", "doc", "targets"), "[[object]] %r" % table.get("name"), path
    )
    name = _required_string(table, "name", "object", path)
    node = parse_type(_required_string(table, "type", "object %s" % name, path), sorted(known))
    is_const = table.get("const", False)
    if not isinstance(is_const, bool):
        raise KabiError("%s: object %s const must be a bool" % (path, name))
    return Object(
        name=name,
        type=node,
        is_const=is_const,
        doc=_doc(table, path, "object %s" % name),
        targets=_targets(table, path, "object %s" % name),
    )


def _check_string_const(text: str, path: str, name: str) -> None:
    """字符串常量只接受可打印 ASCII（无引号 / 反斜杠）：C / Rust 字面量同一份文本。"""
    for char in text:
        if char in ('"', "\\") or not (0x20 <= ord(char) <= 0x7E):
            raise KabiError(
                "%s: const %s value must be printable ASCII without quotes or backslashes"
                % (path, name)
            )


def _build_constant(table: dict, path: str, known: Set[str]) -> Constant:
    _reject_unknown(
        table, ("name", "type", "value", "doc", "targets"), "[[const]] %r" % table.get("name"), path
    )
    name = _required_string(table, "name", "const", path)
    type_text = _required_string(table, "type", "const %s" % name, path).strip()
    if type_text == "string":
        literal = _required_string(table, "value", "const %s" % name, path)
        _check_string_const(literal, path, name)
        return Constant(
            name=name,
            type=Str(),
            literal=literal,
            value=None,
            doc=_doc(table, path, "const %s" % name),
            targets=_targets(table, path, "const %s" % name),
        )
    node = parse_type(type_text, sorted(known))
    if not (isinstance(node, Prim) and node.name in INTEGER_PRIMITIVES):
        raise KabiError(
            "%s: const %s type must be an integer primitive or 'string'" % (path, name)
        )
    literal = _required_string(table, "value", "const %s" % name, path).strip()
    try:
        value = int(literal.replace("_", ""), 0)
    except ValueError:
        raise KabiError("%s: const %s value %r is not an integer literal" % (path, name, literal))
    low, high = RUST_REPR_RANGES[node.name]
    if not low <= value <= high:
        raise KabiError("%s: const %s value %d out of %s range" % (path, name, value, node.name))
    return Constant(
        name=name,
        type=node,
        literal=literal,
        value=value,
        doc=_doc(table, path, "const %s" % name),
        targets=_targets(table, path, "const %s" % name),
    )


def _build_function(table: dict, path: str, known: Set[str]) -> Function:
    _reject_unknown(
        table,
        (
            "name",
            "ret",
            "param",
            "core_param",
            "doc",
            "c_doc",
            "category",
            "c_category",
            "category_group",
            "targets",
        ),
        "[[function]] %r" % table.get("name"),
        path,
    )
    name = _required_string(table, "name", "function", path)
    ret = parse_type(
        _required_string(table, "ret", "function %s" % name, path), sorted(known), allow_void=True
    )
    params = _build_params(table, "param", path, "function %s" % name, known)
    core_params = params
    if _table_list(table, "core_param", path):
        core_params = _build_params(table, "core_param", path, "function %s (core)" % name, known)
        if len(core_params) != len(params):
            raise KabiError(
                "%s: function %s core_param must keep the same arity (%d != %d)"
                % (path, name, len(core_params), len(params))
            )
    doc = _doc(table, path, "function %s" % name)
    c_doc = _doc_key(table, "c_doc", path, "function %s" % name) or doc
    category = _required_string(table, "category", "function %s" % name, path)
    c_category = table.get("c_category", category)
    if not isinstance(c_category, str):
        raise KabiError("%s: function %s c_category must be a string" % (path, name))
    category_group = table.get("category_group", category)
    if not isinstance(category_group, str):
        raise KabiError("%s: function %s category_group must be a string" % (path, name))
    return Function(
        name=name,
        ret=ret,
        params=params,
        core_params=core_params,
        doc=doc,
        c_doc=c_doc,
        category=category,
        c_category=c_category,
        category_group=category_group,
        targets=_targets(table, path, "function %s" % name),
    )


# ===========================================================================
# Emitters：target = C / SDK-Rust / Core-Rust / Core-export-table
# ===========================================================================


def banner_c(source: str) -> str:
    return "/* @generated by %s from %s. DO NOT EDIT. */" % (GENERATOR, source)


def banner_rust(source: str) -> str:
    return "// @generated by %s from %s. DO NOT EDIT." % (GENERATOR, source)


def _banner_sources(schemas: Sequence[Schema]) -> str:
    return ", ".join(schema.banner for schema in schemas)


def _rust_doc(text: str, indent: int = 0) -> str:
    pad = " " * indent
    return "\n".join(pad + ("/// " + line if line else "///") for line in text.split("\n"))


def _c_comment(text: str, indent: int = 0) -> str:
    pad = " " * indent
    parts = text.split("\n")
    if len(parts) == 1:
        return pad + "/* %s */" % parts[0]
    out = [pad + "/* " + parts[0]]
    for part in parts[1:-1]:
        out.append(pad + " * " + part)
    out.append(pad + " * " + parts[-1] + " */")
    return "\n".join(out)


def _c_type_names(schemas: Sequence[Schema]) -> Dict[str, str]:
    names: Dict[str, str] = {}
    for schema in schemas:
        for struct in schema.structs:
            names[struct.name] = "struct %s" % struct.c_name
        for enum in schema.enums:
            names[enum.name] = "enum %s" % enum.c_name
        for alias in schema.aliases:
            names[alias.name] = alias.c_name
    return names


class Emitter:
    """一个 target = 一种语言/消费者视角的渲染器。"""

    target = ""
    language = ""

    def render(self, schemas: Sequence[Schema], output: "Output") -> str:
        raise NotImplementedError


class CEmitter(Emitter):
    target = "c"
    language = "c"

    def render(self, schemas: Sequence[Schema], output: "Output") -> str:
        names = _c_type_names(schemas)
        guard = output.guard
        lines = [
            banner_c(_banner_sources(schemas)),
            "",
            "#ifndef %s" % guard,
            "#define %s" % guard,
            "",
            "#include <stddef.h>",
            "#include <stdint.h>",
            "",
            "#ifdef __cplusplus",
            'extern "C" {',
            "#endif",
        ]
        for schema in schemas:
            for block in self._schema_blocks(schema, names):
                lines += ["", block]
        lines += [
            "",
            "#ifdef __cplusplus",
            "}",
            "#endif",
            "",
            "#endif /* %s */" % guard,
        ]
        return "\n".join(lines) + "\n"

    def _schema_blocks(self, schema: Schema, names: Dict[str, str]) -> List[str]:
        blocks: List[str] = []
        for struct in schema.structs:
            if "c" in struct.targets:
                blocks.append(self._struct(struct, names))
        for alias in schema.aliases:
            if "c" in alias.targets:
                blocks.append(self._alias(alias, names))
        for enum in schema.enums:
            if "c" in enum.targets:
                blocks.append(self._enum(enum))
        for obj in schema.objects:
            if "c" in obj.targets:
                blocks.append(self._object(obj, names))
        for const in schema.constants:
            if "c" in const.targets:
                blocks.append(self._constant(const))
        for entry in schema.entries:
            if "c" in entry.targets:
                blocks.append(self._entry(entry, names))
        functions = [func for func in schema.functions if "c" in func.targets]
        if functions:
            blocks.append(self._functions(functions, names))
        return blocks

    def _struct(self, struct: Struct, names: Dict[str, str]) -> str:
        lines: List[str] = []
        if struct.doc:
            lines.append(_c_comment(struct.doc))
        lines.append("struct %s {" % struct.c_name)
        for field in struct.fields:
            if field.doc:
                lines.append(_c_comment(field.doc, indent=4))
            lines.append("    %s;" % c_decl(field.type, field.name, names))
        lines.append("};")
        for assert_line in _c_layout_asserts(struct):
            lines.append(assert_line)
        return "\n".join(lines)

    def _alias(self, alias: Alias, names: Dict[str, str]) -> str:
        lines: List[str] = []
        if alias.doc:
            lines.append(_c_comment(alias.doc))
        lines.append(c_fn_typedef(alias.ret, alias.params, alias.c_name, names))
        return "\n".join(lines)

    def _enum(self, enum: Enum) -> str:
        if enum.c_style == "defines":
            lines: List[str] = []
            if enum.doc:
                lines.append(_c_comment(enum.doc))
            width = max(len(variant.c_name) for variant in enum.variants)
            for variant in enum.variants:
                if variant.doc:
                    lines.append(_c_comment(variant.doc))
                lines.append("#define %-*s %d" % (width, variant.c_name, variant.value))
            return "\n".join(lines)
        lines = []
        if enum.doc:
            lines.append(_c_comment(enum.doc))
        lines.append("enum %s {" % enum.c_name)
        for variant in enum.variants:
            if variant.doc:
                lines.append(_c_comment(variant.doc, indent=4))
            lines.append("    %s = %d," % (variant.c_name, variant.value))
        lines.append("};")
        return "\n".join(lines)

    def _object(self, obj: Object, names: Dict[str, str]) -> str:
        lines: List[str] = []
        if obj.doc:
            lines.append(_c_comment(obj.doc))
        lines.append("extern %s;" % c_decl(obj.type, obj.name, names, const=obj.is_const))
        return "\n".join(lines)

    def _constant(self, const: Constant) -> str:
        lines: List[str] = []
        if const.doc:
            lines.append(_c_comment(const.doc))
        if isinstance(const.type, Str):
            lines.append('#define %s "%s"' % (const.name, const.literal))
            return "\n".join(lines)
        wrapper = {"u64": "UINT64_C", "u32": "UINT32_C", "u16": "UINT16_C", "u8": "UINT8_C"}
        type_name = const.type.name  # type 已校验为整数 primitive
        # C 没有数字分隔符：schema 里的 `0x..._...`（Rust 侧更易读）在这里去掉下划线。
        literal = const.literal.replace("_", "")
        if type_name in wrapper:
            lines.append("#define %s %s(%s)" % (const.name, wrapper[type_name], literal))
        else:
            lines.append("#define %s %s" % (const.name, literal))
        return "\n".join(lines)

    def _entry(self, entry: Entry, names: Dict[str, str]) -> str:
        lines: List[str] = []
        if entry.doc:
            lines.append(_c_comment(entry.doc))
        lines.append(c_fn_decl(entry.ret, entry.params, entry.name, names) + ";")
        return "\n".join(lines)

    def _functions(self, functions: Sequence[Function], names: Dict[str, str]) -> str:
        lines: List[str] = []
        previous_category = None
        for func in functions:
            if func.c_category != previous_category:
                lines.append("/* -- %s -- */" % func.c_category)
                previous_category = func.c_category
            if func.c_doc:
                lines.append(_c_comment(func.c_doc))
            lines.append(c_fn_decl(func.ret, func.params, func.name, names) + ";")
        return "\n".join(lines)


def _c_layout_asserts(struct: Struct) -> List[str]:
    c_name = struct.c_name
    lines: List[str] = []
    if struct.size_ptrs is not None:
        lines.append(
            '_Static_assert(sizeof(struct %s) == %d * sizeof(void *), "%s layout drift");'
            % (c_name, struct.size_ptrs, c_name)
        )
        lines.append(
            '_Static_assert(_Alignof(struct %s) == _Alignof(void *), "%s alignment drift");'
            % (c_name, c_name)
        )
    if struct.size64 is not None and struct.size32 is not None:
        if struct.size64 == struct.size32:
            lines.append(
                '_Static_assert(sizeof(struct %s) == %d, "%s layout drift");'
                % (c_name, struct.size64, c_name)
            )
        else:
            lines.append("#if __SIZEOF_POINTER__ == 8")
            lines.append(
                '_Static_assert(sizeof(struct %s) == %d, "%s layout drift on RV64");'
                % (c_name, struct.size64, c_name)
            )
            lines.append("#else")
            lines.append(
                '_Static_assert(sizeof(struct %s) == %d, "%s layout drift on RV32");'
                % (c_name, struct.size32, c_name)
            )
            lines.append("#endif")
    if struct.align is not None:
        lines.append(
            '_Static_assert(_Alignof(struct %s) == %d, "%s alignment drift");'
            % (c_name, struct.align, c_name)
        )
    for field in struct.fields:
        if field.offset64 is None or field.offset32 is None:
            continue
        if field.offset64 == field.offset32:
            lines.append(
                '_Static_assert(offsetof(struct %s, %s) == %d, "%s.%s offset drift");'
                % (c_name, field.name, field.offset64, c_name, field.name)
            )
        else:
            lines.append("#if __SIZEOF_POINTER__ == 8")
            lines.append(
                '_Static_assert(offsetof(struct %s, %s) == %d, "%s.%s offset drift on RV64");'
                % (c_name, field.name, field.offset64, c_name, field.name)
            )
            lines.append("#else")
            lines.append(
                '_Static_assert(offsetof(struct %s, %s) == %d, "%s.%s offset drift on RV32");'
                % (c_name, field.name, field.offset32, c_name, field.name)
            )
            lines.append("#endif")
    return lines


class RustEmitter(Emitter):
    language = "rust"
    declares_functions = True

    def render(self, schemas: Sequence[Schema], output: "Output") -> str:
        blocks = [banner_rust(_banner_sources(schemas))]
        for schema in schemas:
            blocks.extend(self._schema_blocks(schema))
        return "\n\n".join(blocks) + "\n"

    def _schema_blocks(self, schema: Schema) -> List[str]:
        blocks: List[str] = []
        for enum in schema.enums:
            if self.target in enum.targets:
                blocks.append(self._enum(enum))
        for struct in schema.structs:
            if self.target in struct.targets:
                blocks.append(self._struct(struct))
        for alias in schema.aliases:
            if self.target in alias.targets:
                blocks.append(self._alias(alias))
        for entry in schema.entries:
            if self.target in entry.targets:
                blocks.append(self._entry(entry))
        for const in schema.constants:
            if self.target in const.targets:
                blocks.append(self._constant(const))
        for obj in schema.objects:
            if self.target in obj.targets:
                blocks.append(self._object(obj))
        if self.declares_functions:
            functions = [func for func in schema.functions if self.target in func.targets]
            if functions:
                blocks.append(self._functions(functions))
        return blocks

    def _enum_decl(self, enum: Enum) -> str:
        lines: List[str] = []
        if enum.doc:
            lines.append(_rust_doc(enum.doc))
        lines.append("#[repr(%s)]" % enum.repr)
        lines.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
        lines.append("pub enum %s {" % enum.name)
        for variant in enum.variants:
            if variant.doc:
                lines.append(_rust_doc(variant.doc, indent=4))
            lines.append("    %s = %d," % (variant.name, variant.value))
        lines.append("}")
        return "\n".join(lines)

    def _enum(self, enum: Enum) -> str:
        return self._enum_decl(enum)

    def _struct(self, struct: Struct) -> str:
        lines: List[str] = []
        if struct.doc:
            lines.append(_rust_doc(struct.doc))
        lines.append("#[repr(C)]")
        lines.append(self._struct_derives(struct))
        lines.append("pub struct %s {" % struct.name)
        for field in struct.fields:
            if field.doc:
                lines.append(_rust_doc(field.doc, indent=4))
            lines.append("    pub %s: %s," % (field.name, _render_rust(field.type)))
        lines.append("}")
        lines.append("")
        lines.append(self._struct_asserts(struct))
        return "\n".join(lines)

    def _struct_derives(self, struct: Struct) -> str:
        derives = ["Debug", "Clone", "Copy"]
        # 函数指针不可有意义地比较（地址不唯一）：不派生 PartialEq / Eq。
        if not any(_contains_fn_pointer(field.type) for field in struct.fields):
            derives += ["PartialEq", "Eq"]
        return "#[derive(%s)]" % ", ".join(derives)

    def _struct_asserts(self, struct: Struct) -> str:
        name = struct.name
        literal_asserts: List[str] = []
        width_asserts: List[str] = []
        if struct.size_ptrs is not None:
            factor = "" if struct.size_ptrs == 1 else "%d * " % struct.size_ptrs
            literal_asserts.append(
                "assert!(core::mem::size_of::<%s>() == %score::mem::size_of::<usize>());"
                % (name, factor)
            )
            literal_asserts.append(
                "assert!(core::mem::align_of::<%s>() == core::mem::align_of::<usize>());" % name
            )
        if struct.size64 is not None and struct.size32 is not None:
            if struct.size64 == struct.size32:
                literal_asserts.append(
                    "assert!(core::mem::size_of::<%s>() == %d);" % (name, struct.size64)
                )
            else:
                width_asserts.append("assert!(core::mem::size_of::<%s>() == %d);" % (name, struct.size64))
        if struct.align is not None:
            literal_asserts.append("assert!(core::mem::align_of::<%s>() == %d);" % (name, struct.align))
        for field in struct.fields:
            if field.offset64 is None or field.offset32 is None:
                continue
            if field.offset64 == field.offset32:
                literal_asserts.append(
                    "assert!(core::mem::offset_of!(%s, %s) == %d);" % (name, field.name, field.offset64)
                )
            else:
                width_asserts.append(
                    "assert!(core::mem::offset_of!(%s, %s) == %d);" % (name, field.name, field.offset64)
                )
        lines = ["const _: () = {"]
        if width_asserts:
            lines.append("    if core::mem::size_of::<usize>() == 8 {")
            lines += ["        " + line for line in width_asserts]
            lines.append("    } else {")
            for field in struct.fields:
                if (
                    field.offset64 is not None
                    and field.offset32 is not None
                    and field.offset64 != field.offset32
                ):
                    lines.append(
                        "        assert!(core::mem::offset_of!(%s, %s) == %d);"
                        % (name, field.name, field.offset32)
                    )
            if struct.size32 is not None and struct.size64 != struct.size32:
                lines.append("        assert!(core::mem::size_of::<%s>() == %d);" % (name, struct.size32))
            lines.append("    }")
        lines += ["    " + line for line in literal_asserts]
        lines.append("};")
        return "\n".join(lines)

    def _alias(self, alias: Alias) -> str:
        lines: List[str] = []
        if alias.doc:
            lines.append(_rust_doc(alias.doc))
        lines.append(
            "pub type %s = %s;"
            % (alias.name, rust_fn_type(alias.ret, alias.params, alias.safe, with_names=True))
        )
        return "\n".join(lines)

    def _entry(self, entry: Entry) -> str:
        lines: List[str] = []
        if entry.doc:
            lines.append(_rust_doc(entry.doc))
        lines.append(
            "pub type %s = %s;"
            % (entry.alias, rust_fn_type(entry.ret, entry.params, safe=True, with_names=True))
        )
        return "\n".join(lines)

    def _constant(self, const: Constant) -> str:
        lines: List[str] = []
        if const.doc:
            lines.append(_rust_doc(const.doc))
        if isinstance(const.type, Str):
            # const 自带 'static；显式写 `&'static [u8]` 会触发
            # clippy::redundant_static_lifetimes。
            lines.append('pub const %s: &[u8] = b"%s";' % (const.name, const.literal))
            return "\n".join(lines)
        lines.append("pub const %s: %s = %s;" % (const.name, _render_rust(const.type), const.literal))
        return "\n".join(lines)

    def _object(self, obj: Object) -> str:
        raise KabiError(
            "%s: object declarations are C-only in this phase (%s)" % (self.target, obj.name)
        )

    def _functions(self, functions: Sequence[Function]) -> str:
        lines = ['unsafe extern "C" {']
        previous_category = None
        for func in functions:
            if func.c_category != previous_category:
                lines.append("    // -- %s --" % func.c_category)
                previous_category = func.c_category
            if func.doc:
                lines.append(_rust_doc(func.doc, indent=4))
            lines.append('    #[link_name = "%s"]' % func.name)
            args = ", ".join("%s: %s" % (param.name, _render_rust(param.type)) for param in func.params)
            ret = _render_rust(func.ret)
            signature = "    pub fn %s(%s)" % (func.name, args)
            if ret != "()":
                signature += " -> " + ret
            lines.append(signature + ";")
        lines.append("}")
        return "\n".join(lines)


class SdkRustEmitter(RustEmitter):
    target = "sdk-rust"

    def _enum(self, enum: Enum) -> str:
        if enum.decode_fallback is None:
            return self._enum_decl(enum)
        lines = [self._enum_decl(enum), "", "impl %s {" % enum.name]
        lines.append("    /// ABI 返回值：`0` 或 `-errno`（配合 `Result` 的 `Err` 侧使用）。")
        lines.append("    pub const fn code(self) -> i32 {")
        lines.append("        -(self as i32)")
        lines.append("    }")
        lines.append("")
        lines.append("    /// 从 ABI 返回值（`0` / `-errno`）解码。未知负码（Core 新加、SDK 未同步）回退到")
        lines.append("    /// [`%s::%s`]——**不 UB、不静默成功**。" % (enum.name, enum.decode_fallback))
        lines.append("    pub const fn from_code(code: i32) -> Self {")
        lines.append("        match -code {")
        for variant in enum.variants:
            lines.append("            %d => Self::%s," % (variant.value, variant.name))
        lines.append("            _ => Self::%s," % enum.decode_fallback)
        lines.append("        }")
        lines.append("    }")
        lines.append("")
        lines.append("    /// 符号名（日志 / 诊断用）。")
        lines.append("    pub const fn name(self) -> &'static str {")
        lines.append("        match self {")
        for variant in enum.variants:
            lines.append('            Self::%s => "%s",' % (variant.name, variant.name))
        lines.append("        }")
        lines.append("    }")
        lines.append("}")
        return "\n".join(lines)


class CoreRustEmitter(RustEmitter):
    target = "core-rust"
    # Core 侧不声明 extern 导出：那些函数由 Core 自己实现，导出表用 typed ref 锚定。
    declares_functions = False

    def _object(self, obj: Object) -> str:
        raise KabiError(
            "%s: object declarations are C-only in this phase (%s)" % (self.target, obj.name)
        )


class CoreExportsEmitter(Emitter):
    target = "core-exports"
    language = "rust"

    def render(self, schemas: Sequence[Schema], output: "Output") -> str:
        functions = [
            func for schema in schemas for func in schema.functions if "core-rust" in func.targets
        ]
        if not functions:
            raise KabiError("%s: no functions to export" % output.path)
        names = [func.name for func in functions]
        if len(set(names)) != len(names):
            raise KabiError("%s: duplicate export table names" % output.path)
        lines = [
            banner_rust(_banner_sources(schemas)),
            "",
            "use crate::generated::abi::*;",
            "use super::{Export, ExportAddress};",
            "",
            "pub(super) static EXPORTS: [Export; %d] = [" % len(functions),
        ]
        previous_category = None
        group_index: Dict[str, int] = {}
        for func in functions:
            if func.category != previous_category:
                if func.category_group in group_index:
                    lines.append(
                        "    // Category %d（续）：%s"
                        % (group_index[func.category_group], func.category)
                    )
                else:
                    group_index[func.category_group] = len(group_index)
                    lines.append(
                        "    // Category %d：%s" % (group_index[func.category_group], func.category)
                    )
                previous_category = func.category
            binding = rust_fn_type(func.ret, func.core_params, safe=True, with_names=False)
            lines.append("    Export {")
            lines.append('        name: b"%s",' % func.name)
            lines.append("        address: ExportAddress({")
            lines.append("            let implementation: %s = super::%s;" % (binding, func.name))
            lines.append("            implementation as *const ()")
            lines.append("        }),")
            lines.append("    },")
        lines.append("];")
        return "\n".join(lines) + "\n"


EMITTERS: Dict[str, type] = {
    "c": CEmitter,
    "sdk-rust": SdkRustEmitter,
    "core-rust": CoreRustEmitter,
    "core-exports": CoreExportsEmitter,
}


# ===========================================================================
# 输出计划
# ===========================================================================


@dataclass(frozen=True)
class Output:
    """一个生成文件：target + 仓库相对路径 + 输入 schema（按顺序拼接）+ C guard。"""

    target: str
    path: str
    inputs: Tuple[str, ...]
    guard: str = ""
    index: bool = True


OUTPUTS: Tuple[Output, ...] = (
    Output(
        "c",
        "os/components/kcomp-sdk/include/generated/errno.h",
        ("errno.toml",),
        guard="KCOMP_GENERATED_ERRNO_H",
    ),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/errno.rs", ("errno.toml",)),
    Output("core-rust", "os/core/src/generated/errno.rs", ("errno.toml",)),
    Output(
        "c",
        "os/components/kcomp-sdk/include/generated/kcomp_abi.h",
        (
            "component.toml",
            "core.toml",
            "block.toml",
            "filesystem.toml",
            "vfs.toml",
            "network.toml",
            "posix.toml",
            "probe.toml",
            "scheduler.toml",
        ),
        guard="KCOMP_GENERATED_ABI_H",
    ),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/abi.rs", ("component.toml", "core.toml")),
    Output(
        "sdk-rust", "os/components/kcomp-sdk/src/generated/block.rs", ("block.toml",)
    ),
    Output(
        "sdk-rust",
        "os/components/kcomp-sdk/src/generated/filesystem.rs",
        ("filesystem.toml",),
    ),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/probe.rs", ("probe.toml",)),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/vfs.rs", ("vfs.toml",)),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/network.rs", ("network.toml",)),
    Output("sdk-rust", "os/components/kcomp-sdk/src/generated/posix.rs", ("posix.toml",)),
    Output(
        "sdk-rust",
        "os/components/kcomp-sdk/src/generated/scheduler.rs",
        ("scheduler.toml",),
    ),
    Output(
        "core-rust",
        "os/core/src/generated/abi.rs",
        ("component.toml", "core.toml", "scheduler.toml"),
    ),
    Output(
        "core-exports",
        "os/core/src/component/generated/exports.rs",
        ("core.toml",),
        index=False,
    ),
)


@dataclass
class GeneratedFile:
    path: str
    language: str
    text: str


def plan(schemas: Sequence[Schema], out_root: str) -> List[GeneratedFile]:
    by_name = {os.path.basename(schema.path): schema for schema in schemas}
    generated: List[GeneratedFile] = []
    module_dirs: Dict[str, List[Tuple[str, str]]] = {}
    used: Set[str] = set()
    for output in OUTPUTS:
        missing = [name for name in output.inputs if name not in by_name]
        if missing:
            raise KabiError(
                "output %s needs schema(s) not passed: %s" % (output.path, ", ".join(missing))
            )
        inputs = [by_name[name] for name in output.inputs]
        used.update(output.inputs)
        emitter = EMITTERS[output.target]()
        generated.append(GeneratedFile(output.path, emitter.language, emitter.render(inputs, output)))
        if output.index and output.path.endswith(".rs"):
            directory, filename = posixpath.split(output.path)
            module_dirs.setdefault(directory, []).append((filename[:-3], _banner_sources(inputs)))
    unused = sorted(set(by_name) - used)
    if unused:
        raise KabiError("schema(s) not used by any output: %s" % ", ".join(unused))
    for directory in sorted(module_dirs):
        entries = sorted(set(module_dirs[directory]))
        body = "\n".join("pub mod %s;" % module for module, _ in entries)
        text = (
            banner_rust(", ".join(sorted(set(banner for _, banner in entries))))
            + "\n\n"
            + body
            + "\n"
        )
        generated.append(GeneratedFile(posixpath.join(directory, "mod.rs"), "rust", text))
    for item in generated:
        if item.language == "rust":
            item.text = _rustfmt(item.text, _crate_edition(os.path.join(out_root, item.path)), out_root)
    return generated


def _rustfmt(text: str, edition: str, working_dir: str) -> str:
    """用仓库钉住的 rustfmt 渲染生成代码：先写逻辑文本，再让 rustfmt 定形。"""
    try:
        proc = subprocess.run(
            ["rustfmt", "--edition", edition, "--emit", "stdout"],
            input=text,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            universal_newlines=True,
            cwd=working_dir,
        )
    except OSError as exc:
        raise KabiError("cannot run rustfmt (%s); Rust targets need the pinned toolchain" % exc)
    if proc.returncode != 0:
        raise KabiError("rustfmt failed for generated Rust:\n%s" % proc.stderr.strip())
    return proc.stdout


def _crate_edition(path: str) -> str:
    """沿输出文件所在目录向上找 Cargo.toml，读 [package] edition（rustfmt 需要）。"""
    directory = os.path.dirname(os.path.abspath(path))
    while True:
        manifest = os.path.join(directory, "Cargo.toml")
        if os.path.isfile(manifest):
            return _package_edition(manifest)
        parent = os.path.dirname(directory)
        if parent == directory:
            raise KabiError("no Cargo.toml above %s; cannot pick a rustfmt edition" % path)
        directory = parent


def _package_edition(manifest: str) -> str:
    """只扫 [package] 段的 `edition = "..."`：Cargo.toml 里有数组/内联表，
    这里没必要（也不应该）为此扩 TOML 解析器。"""
    section = ""
    for line in _read_text(manifest).split("\n"):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped[1:-1].strip()
            continue
        if section == "package":
            match = re.match(r'edition\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    raise KabiError("%s: cannot find [package] edition" % manifest)


# ===========================================================================
# 命令
# ===========================================================================


def _write(path: str, text: str) -> None:
    directory = os.path.dirname(path)
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


def cmd_generate(args) -> int:
    schemas = load_schemas(args.schema)
    for generated in plan(schemas, args.out_root):
        path = os.path.join(args.out_root, generated.path)
        if os.path.isfile(path) and _read_text(path) == generated.text:
            print("unchanged %s" % generated.path)
            continue
        _write(path, generated.text)
        print("wrote     %s" % generated.path)
    return 0


def cmd_check(args) -> int:
    schemas = load_schemas(args.schema)
    generated = plan(schemas, args.out_root)
    errors: List[str] = []
    expected: Dict[str, Set[str]] = {}
    with tempfile.TemporaryDirectory(prefix="kabi-check-") as tmp:
        for item in generated:
            _write(os.path.join(tmp, item.path), item.text)
            directory, filename = posixpath.split(item.path)
            expected.setdefault(directory, set()).add(filename)
            committed = os.path.join(args.out_root, item.path)
            if not os.path.isfile(committed):
                errors.append("missing generated file: %s" % item.path)
                continue
            actual = _read_text(committed)
            if actual != item.text:
                diff = difflib.unified_diff(
                    actual.splitlines(True),
                    item.text.splitlines(True),
                    fromfile=item.path,
                    tofile=item.path + " (regenerated)",
                )
                errors.append("content mismatch: %s\n%s" % (item.path, "".join(list(diff)[:40])))
        for directory in sorted(expected):
            full = os.path.join(args.out_root, directory)
            if not os.path.isdir(full):
                continue
            extras = sorted(set(os.listdir(full)) - expected[directory])
            if extras:
                errors.append("unexpected file(s) in %s: %s" % (directory, ", ".join(extras)))
    if errors:
        sys.stderr.write("kabi-check FAILED:\n\n" + "\n\n".join(errors) + "\n")
        return 1
    print("kabi-check: OK (%d generated files match)" % len(generated))
    return 0


def cmd_selftest(_args) -> int:
    _selftest()
    print("kabi-selftest: OK (type grammar / schema loader / known-answer checks)")
    return 0


def _selftest() -> None:
    # —— 类型语法：接受并正确渲染 ——
    assert parse_type("u32") == Prim("u32")
    assert parse_type("*mut u8") == Ptr(True, Prim("u8"))
    assert parse_type("*const KcompCreateArgs", ("KcompCreateArgs",)) == Ptr(False, Named("KcompCreateArgs"))
    nested = parse_type("*mut *mut u8")
    assert nested == Ptr(True, Ptr(True, Prim("u8")))
    assert render_type(nested, "rust") == "*mut *mut u8"
    assert render_type(nested, "c") == "uint8_t **"
    function = parse_type("fn(*mut void) -> void")
    assert function == Fn((Ptr(True, Prim("void")),), Prim("void"), safe=False)
    assert render_type(function, "rust") == 'unsafe extern "C" fn(*mut ())'
    assert render_type(function, "c") == "void (*)(void *)"
    safe_function = parse_type("safe fn(*mut void) -> void")
    assert render_type(safe_function, "rust") == 'extern "C" fn(*mut ())'
    assert render_type(safe_function, "c") == "void (*)(void *)"
    function_with_args = parse_type("fn(*const void, u64, bool) -> i32")
    assert render_type(function_with_args, "rust") == 'unsafe extern "C" fn(*const (), u64, bool) -> i32'
    assert render_type(parse_type("*const u8"), "c") == "const uint8_t *"
    # `char` 是 C 侧拼写、Rust 侧 `u8`（FatFs `const char *path` ↔ `*const u8`）。
    assert render_type(parse_type("*const char"), "c") == "const char *"
    assert render_type(parse_type("*const char"), "rust") == "*const u8"
    # fn 参数名（不是类型的一部分，C / Rust 声明都渲染出来）。
    named_fn = parse_type("fn(ctx: *mut void, lba: u64) -> i32")
    assert named_fn == Fn(
        (Ptr(True, Prim("void")), Prim("u64")), Prim("i32"), safe=False, names=("ctx", "lba")
    )
    assert (
        c_decl(named_fn, "read", {}) == "int32_t (*read)(void *ctx, uint64_t lba)"
    )
    assert (
        render_type(named_fn, "rust")
        == 'unsafe extern "C" fn(ctx: *mut (), lba: u64) -> i32'
    )
    # C 声明渲染（带名字的 declarator + 命名类型表）
    names = {"KcompCreateArgs": "struct KcompCreateArgs", "IrqHandler": "IrqHandler"}
    assert c_decl(parse_type("*mut *mut void"), "out_state", names) == "void **out_state"
    assert (
        c_decl(parse_type("*const KcompCreateArgs", ("KcompCreateArgs",)), "args", names)
        == "const struct KcompCreateArgs *args"
    )
    assert c_decl(parse_type("usize"), "len", names) == "size_t len"
    assert (
        c_fn_typedef(
            parse_type("void", allow_void=True),
            (Param("arg", parse_type("*mut void")),),
            "KcompTaskEntry",
            names,
        )
        == "typedef void (*KcompTaskEntry)(void *arg);"
    )
    assert (
        rust_fn_type(
            parse_type("void", allow_void=True),
            (Param("ctx", parse_type("*mut void")),),
            True,
            True,
        )
        == 'extern "C" fn(ctx: *mut ())'
    )
    assert (
        rust_fn_type(parse_type("i32"), (Param("a", parse_type("u32")),), False, False)
        == 'unsafe extern "C" fn(u32) -> i32'
    )
    # —— 类型语法：拒绝未知类型 / 语法错误 / 非法 void 位置 ——
    for bad in (
        "u128",
        "void",
        "fn(void) -> void",
        "*const",
        "*mut",
        "* KcompCreateArgs",
        "fn() ->",
        "KcompCreateArgs",
        "fn(u8 u8) -> void",
        "safe u32",
        "safe",
    ):
        try:
            parse_type(bad, ())
        except KabiError:
            continue
        raise KabiError("selftest: type %r must be rejected" % bad)

    # —— schema 回退解析器（与 tomllib 同语义的归一化）——
    inline = """# comment
[[enum]]
name = "Demo"
repr = "i32"
c_style = "defines"
decode_fallback = "EIO"
doc = \"\"\"
line one
line two
\"\"\"

[[enum.variant]]
name = "EIO"
value = 5
doc = "I/O error"
"""
    data = _parse_toml_subset(inline)
    demo = _build_schema(data, "abi/demo.toml", "abi/demo.toml")
    assert demo.enums[0].doc == "line one\nline two"
    assert demo.enums[0].variants == (Variant("EIO", "EIO", 5, "I/O error"),)
    # —— schema 校验：缺 repr / 重复值 / 悬空 fallback 都拒绝 ——
    for bad_text in (
        '[[enum]]\nname = "X"\nc_style = "defines"\n[[enum.variant]]\nname = "EIO"\nvalue = 5\n',
        '[[enum]]\nname = "X"\nrepr = "i32"\nc_style = "defines"\n[[enum.variant]]\nname = "A"\nvalue = 1\n[[enum.variant]]\nname = "B"\nvalue = 1\n',
        '[[enum]]\nname = "X"\nrepr = "i32"\nc_style = "defines"\ndecode_fallback = "NOPE"\n[[enum.variant]]\nname = "A"\nvalue = 1\n',
        '[[enum]]\nname = "X"\nrepr = "i32"\nc_style = "defines"\nbogus = 1\n[[enum.variant]]\nname = "A"\nvalue = 1\n',
        '[[struct]]\nname = "S"\nsize = 8\n[[struct.field]]\nname = "a"\ntype = "u32"\noffset64 = 0\n',
        # size_ptrs 与字面布局 / 非指针字段都不能共存。
        '[[struct]]\nname = "S"\nsize_ptrs = 2\nsize = 16\n[[struct.field]]\nname = "a"\ntype = "fn() -> void"\n',
        '[[struct]]\nname = "S"\nsize_ptrs = 2\n[[struct.field]]\nname = "a"\ntype = "u32"\n[[struct.field]]\nname = "b"\ntype = "fn() -> void"\n',
        # 字符串常量只接受可打印 ASCII（无引号 / 反斜杠）。
        '[[const]]\nname = "N"\ntype = "string"\nvalue = "a\\"b"\n',
    ):
        try:
            _build_schema(_parse_toml_subset(bad_text), "abi/demo.toml", "abi/demo.toml")
        except KabiError:
            continue
        raise KabiError("selftest: invalid schema must be rejected:\n%s" % bad_text)

    # —— size_ptrs 函数表 / string 常量：emitter 输出形状 ——
    demo = _build_schema(
        _parse_toml_subset(
            '[[const]]\nname = "DEMO_NAME"\ntype = "string"\nvalue = "demo.name"\n'
            '[[struct]]\nname = "DemoApi"\nc_name = "demo_api"\nsize_ptrs = 3\n'
            '[[struct.field]]\nname = "a"\ntype = "fn(ctx: *mut void) -> u32"\n'
            '[[struct.field]]\nname = "b"\ntype = "fn(ctx: *mut void, x: u64) -> i32"\n'
            '[[struct.field]]\nname = "n"\ntype = "usize"\n'
        ),
        "abi/demo.toml",
        "abi/demo.toml",
    )
    assert demo.constants[0].type == Str() and demo.constants[0].value is None
    assert demo.structs[0].size_ptrs == 3
    c_struct = CEmitter()._struct(demo.structs[0], {})
    assert "sizeof(struct demo_api) == 3 * sizeof(void *)" in c_struct
    assert "_Alignof(struct demo_api) == _Alignof(void *)" in c_struct
    assert "uint32_t (*a)(void *ctx);" in c_struct
    assert "size_t n;" in c_struct
    rust_struct = RustEmitter()._struct(demo.structs[0])
    assert "size_of::<DemoApi>() == 3 * core::mem::size_of::<usize>()" in rust_struct
    assert 'pub a: unsafe extern "C" fn(ctx: *mut ()) -> u32,' in rust_struct
    assert "pub n: usize," in rust_struct
    # 函数指针不可比较：不派生 PartialEq / Eq（否则 unpredictable_function_pointer_comparisons）。
    assert "#[derive(Debug, Clone, Copy)]" in rust_struct
    assert "PartialEq" not in rust_struct
    assert CEmitter()._constant(demo.constants[0]) == '#define DEMO_NAME "demo.name"'
    assert (
        RustEmitter()._constant(demo.constants[0])
        == 'pub const DEMO_NAME: &[u8] = b"demo.name";'
    )

    # —— 提交的 errno schema 必须可加载且值集合正确 ——
    schema = load_schema("abi/errno.toml")
    assert len(schema.enums) == 1 and not schema.structs
    errno = schema.enums[0]
    assert errno.name == "Errno" and len(errno.variants) == 131
    assert (errno.variants[0].name, errno.variants[0].value) == ("EPERM", 1)
    assert (errno.variants[-1].name, errno.variants[-1].value) == ("EHWPOISON", 133)
    values = [variant.value for variant in errno.variants]
    assert 41 not in values and 58 not in values
    errno_names = [variant.name for variant in errno.variants]
    assert "ENOTSUP" in errno_names and "EOPNOTSUPP" not in errno_names

    # —— core / component schema：known-answer checks ——
    component, core = load_schemas(["abi/component.toml", "abi/core.toml"])
    assert len(core.functions) == 60
    assert {"kcore_task_start_on", "kcore_cpu_current"} <= {func.name for func in core.functions}
    assert [func.name for func in core.functions][:4] == [
        "kcore_trace_read",
        "kcore_trace_stats",
        "kcore_now",
        "kcore_timebase_hz",
    ]
    assert len([func for func in core.functions if len(func.core_params) != len(func.params)]) == 0
    assert len([func for func in core.functions if func.core_params != func.params]) == 3
    assert len(core.structs) == 6 and len(component.structs) == 2
    user_trap = next(struct for struct in core.structs if struct.name == "UserTrap")
    assert user_trap.size64 == 88 and user_trap.size32 == 88 and user_trap.align == 8
    posix = load_schema("abi/posix.toml")
    process_api = next(struct for struct in posix.structs if struct.name == "PosixProcessApi")
    one_pointer = RustEmitter()._struct(process_api)
    assert "== core::mem::size_of::<usize>()" in one_pointer
    assert "1 *" not in one_pointer
    frame = [struct for struct in component.structs if struct.name == "KcompCallFrame"][0]
    assert frame.c_name == "kcomp_call_frame" and frame.size_ptrs == 6
    assert [field.name for field in frame.fields] == [
        "args",
        "args_len",
        "input",
        "input_len",
        "output",
        "output_len",
    ]
    assert component.entries[0].alias == "KcompInstanceCreate"
    assert component.entries[2].name == "kcomp_service_dispatch"
    assert component.entries[2].alias == "KcompServiceDispatch"
    assert component.aliases[0].name == "KcompTaskEntry"
    assert component.objects[0].name == "kcomp_abi" and component.objects[0].is_const
    assert component.enums[0].c_name == "KcompInterfaceKind"
    kcomp_abi = [const for const in component.constants if const.name == "KCOMP_ABI"][0]
    assert kcomp_abi.value == 0x9D73_405B_B2F8_16C0
    kinds = [const for const in core.constants if const.name.startswith("KIND_")]
    assert [const.value for const in kinds] == list(range(1, 12))
    absent = [const for const in core.constants if const.name == "ABSENT"][0]
    assert absent.value == 2 ** 64 - 1

    # —— block / filesystem schema：组件间契约（function table + 常量）——
    block, filesystem = load_schemas(["abi/block.toml", "abi/filesystem.toml"])
    assert len(block.structs) == 1 and block.structs[0].size_ptrs == 3
    assert [field.name for field in block.structs[0].fields] == [
        "capacity_sectors",
        "read",
        "write",
    ]
    block_abi = [const for const in block.constants if const.name == "KCOMP_BLOCK_DEVICE_ABI"][0]
    assert block_abi.value == 0x424C_4F43_4B44_4556
    block_consts = {const.name: const.value for const in block.constants}
    assert block_consts["KCOMP_BLOCK_DEVICE_CONTRACT"] == 0x424C_4B43_4F4E_5452
    assert block_consts["KCOMP_BLOCK_METHOD_CAPACITY"] == 0
    assert block_consts["KCOMP_BLOCK_METHOD_READ"] == 1
    assert block_consts["KCOMP_BLOCK_METHOD_WRITE"] == 2
    assert block_consts["KCOMP_BLOCK_LBA_LEN"] == 8
    assert block_consts["KCOMP_BLOCK_CAPACITY_LEN"] == 8
    assert len(filesystem.structs) == 1 and filesystem.structs[0].size_ptrs == 5
    assert [field.name for field in filesystem.structs[0].fields] == [
        "mount",
        "unmount",
        "open",
        "close",
        "read",
    ]
    filesystem_abi = [
        const for const in filesystem.constants if const.name == "KCOMP_FILESYSTEM_ABI"
    ][0]
    assert filesystem_abi.value == 0x4649_4C45_5359_5354
    filesystem_consts = {const.name: const.value for const in filesystem.constants}
    assert filesystem_consts["KCOMP_FILESYSTEM_CONTRACT"] == 0x5646_5343_4F4E_5452
    assert filesystem_consts["KCOMP_FILESYSTEM_METHOD_MOUNT"] == 0
    assert filesystem_consts["KCOMP_FILESYSTEM_METHOD_UNMOUNT"] == 1
    assert filesystem_consts["KCOMP_FILESYSTEM_METHOD_OPEN"] == 2
    assert filesystem_consts["KCOMP_FILESYSTEM_METHOD_CLOSE"] == 3
    assert filesystem_consts["KCOMP_FILESYSTEM_METHOD_READ"] == 4
    assert filesystem_consts["KCOMP_FILESYSTEM_HANDLE_LEN"] == 8
    assert filesystem_consts["KCOMP_FILESYSTEM_FLAGS_LEN"] == 4
    assert filesystem_consts["KCOMP_FILESYSTEM_READ_HEADER_LEN"] == 8
    assert filesystem_consts["KCOMP_FILESYSTEM_PATH_MAX"] == 256

    # —— scheduler schema：Gate-only 组件间契约（无 function table / 无结构）——
    scheduler = load_schema("abi/scheduler.toml")
    assert not scheduler.structs and not scheduler.aliases and not scheduler.entries
    assert not scheduler.functions
    scheduler_consts = {const.name: const.value for const in scheduler.constants}
    scheduler_literals = {const.name: const.literal for const in scheduler.constants}
    assert scheduler_literals["KCOMP_SCHEDULER_POLICY_NAME"] == "scheduler.policy"
    assert scheduler_consts["KCOMP_SCHEDULER_POLICY_ABI"] == 0x5343_4845_4443_5055
    assert scheduler_consts["KCOMP_SCHEDULER_POLICY_CONTRACT"] == 0x5343_4845_4450_4F4C
    assert scheduler_consts["KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT"] == 0
    assert scheduler_consts["KCOMP_SCHEDULER_TASK_ID_LEN"] == 4
    assert scheduler_consts["KCOMP_SCHEDULER_ARGS_LEN"] == 8
    assert scheduler_consts["KCOMP_SCHEDULER_NONE"] == 0xFFFF_FFFF


# ===========================================================================
# CLI
# ===========================================================================


def parse_args(argv: Sequence[str]):
    parser = argparse.ArgumentParser(
        prog="kabi_gen.py",
        description="KaleidOS KABI generator: render ABI schemas (abi/*.toml) into C / Rust.",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    for name, help_text in (
        ("generate", "write the generated files for the given schemas"),
        ("check", "regenerate into a temporary directory and diff against the committed files"),
    ):
        sub = commands.add_parser(name, help=help_text)
        sub.add_argument(
            "--schema",
            action="append",
            required=True,
            metavar="PATH",
            help="schema file relative to the repository root (repeatable, e.g. abi/errno.toml)",
        )
        sub.add_argument(
            "--out-root",
            default=".",
            metavar="DIR",
            help="root directory the generated paths are relative to (default: .)",
        )

    commands.add_parser("selftest", help="run generator self-tests (type grammar / schema loader)")
    return parser.parse_args(argv)


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    handlers = {"generate": cmd_generate, "check": cmd_check, "selftest": cmd_selftest}
    try:
        return handlers[args.command](args)
    except KabiError as exc:
        sys.stderr.write("kabi: error: %s\n" % exc)
        return 2


if __name__ == "__main__":
    sys.exit(main())
