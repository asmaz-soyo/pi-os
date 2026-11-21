#!/bin/sh
# MicroPython の Makefile 用:zig cc で aarch64-linux-musl 向けにコンパイルする
exec "${ZIG:-zig}" cc -target aarch64-linux-musl "$@"
