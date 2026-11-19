use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// A size that accepts either pixels (a bare number, logical px) or a
/// percent of the window (a string like `"60%"`, 0–100).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeSpec {
    Px(f32),
    Percent(f32),
}

impl<'de> Deserialize<'de> for SizeSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = SizeSpec;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a pixel number or a percent string like \"60%\"")
            }
            fn visit_i64<E>(self, v: i64) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Px(v as f32))
            }
            fn visit_f64<E>(self, v: f64) -> Result<SizeSpec, E> {
                Ok(SizeSpec::Px(v as f32))
            }
            fn visit_str<E>(self, v: &str) -> Result<SizeSpec, E>
            where
                E: de::Error,
            {
                let trimmed = v.trim();
                let Some(number) = trimmed.strip_suffix('%') else {
                    return Err(E::custom(format!(
                        "expected a percent like \"60%\", got {v:?}"
                    )));
                };
                let percent: f32 = number
                    .trim()
                    .parse()
                    .map_err(|_| E::custom(format!("invalid percent {v:?}")))?;
                Ok(SizeSpec::Percent(percent.clamp(0.0, 100.0)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Serialize for SizeSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            SizeSpec::Px(px) => serializer.serialize_f32(*px),
            SizeSpec::Percent(percent) => {
                serializer.serialize_str(&format!("{percent}%"))
            }
        }
    }
}

/// `[input-bar]` — geometry of the unified input bar. `width` is
/// centered horizontally; `height` reserves terminal layout space and
/// falls back to six terminal lines (text at twice the terminal font)
/// when unset.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InputBarConfig {
    #[serde(default = "default_width")]
    pub width: SizeSpec,
    #[serde(default)]
    pub height: Option<SizeSpec>,
}

impl Default for InputBarConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: None,
        }
    }
}

#[inline]
fn default_width() -> SizeSpec {
    SizeSpec::Percent(100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Config {
        input_bar: InputBarConfig,
    }

    #[test]
    fn defaults_are_full_width_six_lines() {
        // An empty section exercises the field defaults; the section
        // itself is optional at the Config level via serde(default).
        let config = toml::from_str::<Config>("[input_bar]").unwrap();
        assert_eq!(config.input_bar.width, SizeSpec::Percent(100.0));
        assert_eq!(config.input_bar.height, None);
    }

    #[test]
    fn pixels_and_percent_both_parse() {
        let config = toml::from_str::<Config>(
            r#"
            [input_bar]
            width = "60%"
            height = 160
            "#,
        )
        .unwrap();
        assert_eq!(config.input_bar.width, SizeSpec::Percent(60.0));
        assert_eq!(config.input_bar.height, Some(SizeSpec::Px(160.0)));

        let config = toml::from_str::<Config>(
            r#"
            [input_bar]
            width = 900
            height = "30%"
            "#,
        )
        .unwrap();
        assert_eq!(config.input_bar.width, SizeSpec::Px(900.0));
        assert_eq!(config.input_bar.height, Some(SizeSpec::Percent(30.0)));
    }

    #[test]
    fn percent_is_clamped_and_bad_strings_rejected() {
        let config = toml::from_str::<Config>(
            r#"
            [input_bar]
            width = "150%"
            "#,
        )
        .unwrap();
        assert_eq!(config.input_bar.width, SizeSpec::Percent(100.0));

        assert!(toml::from_str::<Config>(
            r#"
            [input_bar]
            width = "60"
            "#
        )
        .is_err());
    }
}
