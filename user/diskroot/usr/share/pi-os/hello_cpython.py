# CPython のテスト(QEMU の結合テストが動かす)
import collections
import json
import os
import re
import sys

print("Hello from CPython", sys.version.split()[0], "on", sys.platform)
words = re.findall(r"\w+", "the quick brown fox jumps over the lazy dog the end")
print("most common:", collections.Counter(words).most_common(1))
print(json.dumps({"pid_positive": os.getpid() > 0, "cwd": os.getcwd()}))
with open("/tmp/cpython.txt", "w") as f:
    f.write("written by CPython\n")
print(open("/tmp/cpython.txt").read().strip())
sys.exit(7)
