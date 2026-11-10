//! The icons: gpui-kit's default set, plus the few Lucide icons the manager adds (ISC licence,
//! bundled by gpui-kit-assets).
use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(ExtraIcons, [Pencil, Trash, Key, Cloud, Plug]);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit::assets::Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_omarchy::IconName;

    #[test]
    fn every_icon_the_manager_uses_loads() {
        for name in [
            IconName::Plus,
            IconName::Pencil,
            IconName::Trash,
            IconName::Key,
            IconName::Cloud,
            IconName::Plug,
            IconName::ArrowUp,
            IconName::ArrowDown,
            IconName::CircleCheck,
            IconName::CircleAlert,
            IconName::LoaderCircle,
        ] {
            assert!(
                Assets.load(name.path().as_ref()).unwrap().is_some(),
                "{name:?}"
            );
        }
    }
}
