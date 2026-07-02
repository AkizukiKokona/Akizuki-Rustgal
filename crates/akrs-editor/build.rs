//! 构建脚本：在 Windows 上将自定义图标嵌入可执行文件。
//!
//! 从 `assets/icon_kokona_64.bin`（原始 RGBA 数据）生成 .ico 文件，
//! 然后使用 `winres` 将其链接进 exe 的资源段。
//! 非 Windows 平台上此脚本不做任何操作。

use std::io::Write;
use std::path::Path;

fn main() {
    // 仅在目标平台为 Windows 时执行
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // 读取原始 RGBA 图标数据（64x64x4 = 16384 字节）
    let bin_path = Path::new("../../assets/icon_kokona_64.bin");
    let rgba_data = match std::fs::read(bin_path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("cargo:warning=无法读取图标数据文件，跳过 exe 图标嵌入");
            return;
        }
    };

    const W: usize = 64;
    const H: usize = 64;
    if rgba_data.len() != W * H * 4 {
        eprintln!(
            "cargo:warning=图标数据大小不匹配 ({} != {}), 跳过 exe 图标嵌入",
            rgba_data.len(),
            W * H * 4
        );
        return;
    }

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".to_string());
    let ico_path = Path::new(&out_dir).join("akrs_editor.ico");

    if let Err(e) = write_ico(&ico_path, &rgba_data, W, H) {
        eprintln!("cargo:warning=生成 .ico 文件失败: {}", e);
        return;
    }

    // 使用 winres 将图标嵌入 Windows 可执行文件
    let mut res = winres::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.set("FileDescription", "Akizuki*Rustgal 剧本编辑器");
    res.set("ProductName", "Akizuki*Rustgal");
    res.set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        eprintln!("cargo:warning=编译 Windows 资源失败: {}", e);
    }

    println!("cargo:rerun-if-changed=../../assets/icon_kokona_64.bin");
}

/// 将原始 RGBA 像素数据写入标准 .ico 文件。
///
/// ICO 格式（单张 32-bit 图像）：
/// - ICONDIR (6 bytes)
/// - ICONDIRENTRY (16 bytes)
/// - BITMAPINFOHEADER (40 bytes)
/// - XOR mask: BGRA 像素数据，自底向上
/// - AND mask: 1-bit 透明度掩码（全零 = 完全不透明）
fn write_ico(path: &Path, rgba: &[u8], w: usize, h: usize) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;

    let xor_size = w * h * 4;
    let and_size = w * h / 8;
    let bitmap_size = 40 + xor_size + and_size;

    // --- ICONDIR (6 bytes) ---
    f.write_all(&[0, 0])?; // reserved = 0
    f.write_all(&1u16.to_le_bytes())?; // type = 1 (icon)
    f.write_all(&1u16.to_le_bytes())?; // count = 1

    // --- ICONDIRENTRY (16 bytes) ---
    f.write_all(&[w as u8])?; // width (0 = 256)
    f.write_all(&[h as u8])?; // height (0 = 256)
    f.write_all(&[0u8])?; // colorCount
    f.write_all(&[0u8])?; // reserved
    f.write_all(&1u16.to_le_bytes())?; // planes
    f.write_all(&32u16.to_le_bytes())?; // bitCount
    f.write_all(&(bitmap_size as u32).to_le_bytes())?; // bytesInRes
    f.write_all(&22u32.to_le_bytes())?; // imageOffset = 6 + 16

    // --- BITMAPINFOHEADER (40 bytes) ---
    f.write_all(&40u32.to_le_bytes())?; // biSize
    f.write_all(&(w as i32).to_le_bytes())?; // biWidth
    f.write_all(&((h * 2) as i32).to_le_bytes())?; // biHeight (doubled: XOR + AND)
    f.write_all(&1u16.to_le_bytes())?; // biPlanes
    f.write_all(&32u16.to_le_bytes())?; // biBitCount
    f.write_all(&0u32.to_le_bytes())?; // biCompression
    f.write_all(&0u32.to_le_bytes())?; // biSizeImage
    f.write_all(&0i32.to_le_bytes())?; // biXPelsPerMeter
    f.write_all(&0i32.to_le_bytes())?; // biYPelsPerMeter
    f.write_all(&0u32.to_le_bytes())?; // biClrUsed
    f.write_all(&0u32.to_le_bytes())?; // biClrImportant

    // --- XOR mask: BGRA, bottom-up ---
    for y in (0..h).rev() {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let r = rgba[i];
            let g = rgba[i + 1];
            let b = rgba[i + 2];
            let a = rgba[i + 3];
            f.write_all(&[b, g, r, a])?;
        }
    }

    // --- AND mask: all zeros (fully opaque) ---
    let and_mask = vec![0u8; and_size];
    f.write_all(&and_mask)?;

    Ok(())
}
