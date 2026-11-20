# M3 メモリ管理

## 目的

MMU、物理ページの割り当て、カーネルヒープ、higher half 配置。

## 完了条件

- [x] 物理ページの割り当て(参照カウント付き)
- [x] ページテーブル(4KiB / 2MiB、属性、変換、解除)
- [x] MMU の有効化と higher half 配置(両 BSP、`kernel8.img` でも起動)
- [x] カーネルヒープ(`alloc`)

## 学んだこと

`docs/notes/m3-*.md` を参照。
