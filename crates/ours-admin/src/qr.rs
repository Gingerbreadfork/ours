//! The terminal QR code for the admin link.

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

/// Renders `text` as a QR code in Unicode half-blocks, one string per
/// terminal line. Light modules are drawn, which scans on a dark terminal.
pub fn qr_lines_for(text: &str) -> Vec<String> {
    let code = match QrCode::new(text.as_bytes()) {
        Ok(code) => code,
        Err(error) => {
            tracing::warn!(%error, "cannot render the admin QR code");
            return Vec::new();
        }
    };
    code.render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_renders_as_many_lines_of_blocks() {
        let lines = qr_lines_for("http://192.168.1.20:7878/?token=abcdefghijklmnopqrstuvwxyz");
        assert!(lines.len() > 10, "got {} lines", lines.len());
        assert!(lines.iter().any(|l| l.contains('\u{2588}')));
        let width = lines[0].chars().count();
        assert!(lines.iter().all(|l| l.chars().count() == width));
    }
}
