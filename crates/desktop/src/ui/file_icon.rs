//! Draws file and folder icons in the theme the settings chose for the
//! current light or dark mode: the interface's Lucide icons, or Catppuccin's
//! colored ones in one of its flavors.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::{
    AnyElement, Image, ImageFormat, IntoElement, Styled as _, img, rems,
};

use crate::file::icons::{FileIconCatalog, Flavor};
use crate::state::FileIconTheme;

/// Decoded Catppuccin icons by name and flavor, shared by every painter.
///
/// Names borrow from the static catalog, so a lookup allocates nothing.
pub type FileIconCache =
    Rc<RefCell<HashMap<(&'static str, Flavor), Arc<Image>>>>;

/// Paints file and folder icons; cheap to clone into a row-rendering closure.
#[derive(Clone)]
pub struct FileIconPainter {
    /// Catppuccin flavor to draw in; `None` draws the Lucide icons.
    flavor: Option<Flavor>,
    /// Icons already decoded, so a row redraw does not rebuild its image.
    cache: FileIconCache,
}

impl FileIconPainter {
    /// A painter drawing `theme`.
    pub fn new(theme: FileIconTheme, cache: FileIconCache) -> Self {
        Self {
            flavor: theme.flavor(),
            cache,
        }
    }

    /// The icon of a file named `name`.
    pub fn file(&self, name: &str) -> AnyElement {
        match self.flavor {
            None => Icon::new(IconName::FileText).small().into_any_element(),
            Some(flavor) => {
                self.colored(flavor, FileIconCatalog::builtin().file(name))
            }
        }
    }

    /// The icon of a folder named `name`, open or closed.
    pub fn folder(&self, name: &str, expanded: bool) -> AnyElement {
        match self.flavor {
            None => Icon::new(if expanded {
                IconName::FolderOpen
            } else {
                IconName::Folder
            })
            .small()
            .into_any_element(),
            Some(flavor) => self.colored(
                flavor,
                FileIconCatalog::builtin().folder(name, expanded),
            ),
        }
    }

    /// One Catppuccin icon at the size of a small icon; an unknown name draws nothing.
    fn colored(&self, flavor: Flavor, icon: &'static str) -> AnyElement {
        let key = (icon, flavor);
        let cached = self.cache.borrow().get(&key).cloned();
        let image = cached.or_else(|| {
            let svg = FileIconCatalog::builtin().svg(icon)?;
            let image = Arc::new(Image::from_bytes(
                ImageFormat::Svg,
                flavor.recolor(svg).into_bytes(),
            ));
            self.cache.borrow_mut().insert(key, Arc::clone(&image));
            Some(image)
        });
        match image {
            Some(image) => {
                img(image).size(rems(1.)).flex_none().into_any_element()
            }
            None => gpui_kit::div().size(rems(1.)).into_any_element(),
        }
    }
}
