//! The two languages of the window.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ru,
    En,
}

impl Lang {
    /// Picks the text for this language. Both texts sit together at the place
    /// of use, so a new string cannot miss a translation.
    pub fn tr<'a>(self, ru: &'a str, en: &'a str) -> &'a str {
        match self {
            Lang::Ru => ru,
            Lang::En => en,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_comes_first_and_is_the_default() {
        assert_eq!(Lang::default(), Lang::Ru);
        assert_eq!(Lang::Ru.tr("да", "yes"), "да");
        assert_eq!(Lang::En.tr("да", "yes"), "yes");
    }
}
