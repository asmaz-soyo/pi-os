# MicroPython のテスト(QEMU の結合テストと、シェルからの手動確認用)
import os
import sys

print("Hello from MicroPython", sys.implementation.version[:2])
squares = [n * n for n in range(10)]
print("squares:", squares)
with open("/tmp/py.txt", "w") as f:
    f.write("written by python\n")
print("read back:", open("/tmp/py.txt").read().strip())
print("etc has motd:", "motd" in os.listdir("/etc"))
sys.exit(3)
