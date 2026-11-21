#!/bin/sh
# MicroPython の Makefile 用(CPP):zig cc -E を、1 回に 1 つの .c ファイルで呼ぶ。
# zig は .c をまとめて渡されたり、標準入力(-)から読んだりすると、aarch64-linux-musl の
# ヘッダではなくホストの /usr/include を探してしまうため。
cpp() { "${ZIG:-zig}" cc -target aarch64-linux-musl -E "$@"; }
files=""
tmp=""
for a in "$@"; do
  case "$a" in
    *.c) files="$files $a" ;;
    -)
      tmp=$(mktemp --suffix=.c)
      cat > "$tmp"
      files="$files $tmp" ;;
  esac
done
if [ -z "$files" ]; then
  cpp "$@"
  exit
fi
# ファイル以外の引数(フラグ)だけを残す
for a in "$@"; do
  shift
  case "$a" in
    *.c | -) ;;
    *) set -- "$@" "$a" ;;
  esac
done
status=0
for f in $files; do
  cpp "$@" "$f" || status=1
done
[ -n "$tmp" ] && rm -f "$tmp"
exit $status
