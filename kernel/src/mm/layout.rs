//! カーネルのアドレス空間の設計図。
//!
//! RAM 全体とデバイスを線形マップ(物理アドレス + 一定のずれ)で見えるようにする。
//! カーネルのイメージ(コードとデータ)だけを実行可能にし、残りの RAM は実行不可にする。

pub use core::ops::Range;
use hal::addr::{PAGE_SIZE, PhysAddr};
use hal::mmu::MapFlags;

/// 物理アドレス `pa` から `len` バイトを `flags` でマップする、という1項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub pa: PhysAddr,
    pub len: u64,
    pub flags: MapFlags,
}

fn page_down(x: u64) -> u64 {
    x & !(PAGE_SIZE - 1)
}

fn page_up(x: u64) -> u64 {
    (x + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// カーネルのページテーブルに載せるマッピングの一覧。
pub fn kernel_mappings<'a>(
    ram: Range<u64>,
    image: Range<u64>,
    devices: &'a [Range<u64>],
) -> impl Iterator<Item = Mapping> + 'a {
    let (img_start, img_end) = (page_down(image.start), page_up(image.end));
    let parts = [
        (ram.start, img_start, MapFlags::KERNEL_RW),
        (img_start, img_end, MapFlags::KERNEL_RWX),
        (img_end, ram.end, MapFlags::KERNEL_RW),
    ];
    let ram_parts =
        parts
            .into_iter()
            .filter(|(start, end, _)| end > start)
            .map(|(start, end, flags)| Mapping {
                pa: PhysAddr(start),
                len: end - start,
                flags,
            });
    let device_parts = devices.iter().map(|d| Mapping {
        pa: PhysAddr(page_down(d.start)),
        len: page_up(d.end) - page_down(d.start),
        flags: MapFlags::DEVICE,
    });
    ram_parts.chain(device_parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::addr::PhysAddr;
    use hal::mmu::MapFlags;
    use std::vec::Vec;

    fn plan(ram: Range<u64>, image: Range<u64>, devices: &[Range<u64>]) -> Vec<Mapping> {
        kernel_mappings(ram, image, devices).collect()
    }

    #[test]
    fn ram_is_split_around_the_kernel_image() {
        let m = plan(0x4000_0000..0x4800_0000, 0x4008_0000..0x400a_3000, &[]);
        assert_eq!(
            m,
            [
                Mapping {
                    pa: PhysAddr(0x4000_0000),
                    len: 0x8_0000,
                    flags: MapFlags::KERNEL_RW
                },
                Mapping {
                    pa: PhysAddr(0x4008_0000),
                    len: 0x2_3000,
                    flags: MapFlags::KERNEL_RWX
                },
                Mapping {
                    pa: PhysAddr(0x400a_3000),
                    len: 0x4800_0000 - 0x400a_3000,
                    flags: MapFlags::KERNEL_RW
                },
            ]
        );
    }

    #[test]
    fn image_bounds_are_rounded_to_pages() {
        let m = plan(0x0..0x100_0000, 0x8_0010..0x9_0fff, &[]);
        assert_eq!(m[1].pa, PhysAddr(0x8_0000));
        assert_eq!(m[1].len, 0x1_1000);
        assert_eq!(m[2].pa, PhysAddr(0x9_1000));
    }

    #[test]
    fn empty_ram_part_before_the_image_is_skipped() {
        let m = plan(0x8_0000..0x100_0000, 0x8_0000..0x9_0000, &[]);
        assert_eq!(m[0].flags, MapFlags::KERNEL_RWX);
        assert_eq!(m.len(), 2);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // デバイスの範囲の一覧(たまたま1つ)
    fn devices_are_mapped_as_device_memory() {
        let m = plan(
            0x4000_0000..0x4800_0000,
            0x4008_0000..0x4009_0000,
            &[0x0800_0000..0x0a00_0000],
        );
        assert_eq!(
            m.last(),
            Some(&Mapping {
                pa: PhysAddr(0x0800_0000),
                len: 0x200_0000,
                flags: MapFlags::DEVICE
            })
        );
    }
}
