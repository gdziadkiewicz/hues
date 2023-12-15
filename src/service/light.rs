use super::{
    device::ProductArchetype,
    resource::{ResourceIdentifier, ResourceType},
};
use crate::{
    api::{BridgeClient, HueAPIError},
    command::{merge_commands, LightCommand},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub struct Light<'a> {
    api: &'a BridgeClient,
    data: LightData,
}

impl<'a> Light<'a> {
    pub fn new(api: &'a BridgeClient, data: LightData) -> Self {
        Light { api, data }
    }

    pub fn data(&self) -> &LightData {
        &self.data
    }

    pub fn id(&self) -> &String {
        &self.data.id
    }

    pub fn is_on(&self) -> bool {
        self.data.on.on
    }

    pub fn supports_color(&self) -> bool {
        self.data.color.is_some()
    }

    pub async fn identify(&self) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        self.send(&[LightCommand::Identify]).await
    }

    pub async fn alert(&self) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        self.send(&[LightCommand::Alert(AlertEffectType::Breathe)])
            .await
    }

    pub async fn on(&self) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        self.send(&[LightCommand::On(true)]).await
    }

    pub async fn off(&self) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        self.send(&[LightCommand::On(false)]).await
    }

    pub async fn send(
        &self,
        commands: &[LightCommand],
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let payload = merge_commands(commands);
        self.api.put_light(self.id(), &payload).await
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct LightData {
    /// Unique identifier representing a specific resource instance.
    pub id: String,
    /// Clip v1 resource identifier.
    pub id_v1: Option<String>,
    /// Owner of the service, in case the owner service is deleted, the service also gets deleted.
    pub owner: ResourceIdentifier,
    /// Deprecated: use metadata on device level.
    #[deprecated]
    pub metadata: LightMetadata,
    pub on: OnState,
    pub dimming: DimmingState,
    pub color_temperature: ColorTempState,
    pub color: Option<ColorState>,
    pub dynamics: DynamicsState,
    pub alert: AlertState,
    /// Feature containing signaling properties.
    pub signaling: SignalingState,
    pub mode: Mode,
    /// Basic feature containing gradient properties.
    pub gradient: Option<GradientState>,
    /// Basic feature containing effect properties.
    pub effects: Option<EffectState>,
    /// Basic feature containing timed effect properties.
    pub timed_effects: Option<TimedEffectState>,
    /// Feature containing properties to configure powerup behaviour of a lightsource.
    pub powerup: Option<PowerupState>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LightMetadata {
    /// Human readable name of a resource.
    pub name: String,
    /// Product archetype.
    pub archetype: ProductArchetype,
    /// A fixed mired value of the white lamp.
    pub fixed_mired: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnState {
    /// On/Off state of the light.
    ///
    /// on=true
    /// off=false
    pub on: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DimmingState {
    /// Brightness percentage.
    ///
    /// Value cannot be `0`, writing `0` changes it to lowest possible brightness.
    pub brightness: f32,
    /// Percentage of the maximum lumen the device outputs on minimum brightness.
    pub min_dim_level: Option<f32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ColorTempState {
    /// Color temperature in mirek or [None] when the light color is not in the ct spectrum.
    pub mirek: Option<u16>,
    /// Indication whether the value presented in mirek is valid.
    pub mirek_valid: bool,
    pub mirek_schema: MirekSchema,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MirekSchema {
    /// Minimum color temperature this light supports.
    pub mirek_minimum: u16,
    /// Maximum color temperature this light supports.
    pub mirek_maximum: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ColorState {
    /// CIE XY gamut position
    pub xy: CIEColor,
    pub gamut: CIEGamut,
    /// The gammut types supported by hue.
    ///
    /// – A Gamut of early Philips color-only products
    /// – B Limited gamut of first Hue color products
    /// – C Richer color gamut of Hue white and color ambiance products
    /// – Other Color gamut of non-hue products with non-hue gamuts resp w/o gamut
    pub gamut_type: GamutType,
}

/// Color gamut of color bulb.
/// Some bulbs do not properly return the Gamut information. In this case this is not present.
#[derive(Clone, Debug, Deserialize)]
pub struct CIEGamut {
    /// CIE XY gamut position
    pub red: CIEColor,
    /// CIE XY gamut position
    pub green: CIEColor,
    /// CIE XY gamut position
    pub blue: CIEColor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CIEColor {
    /// X position in color gamut
    pub x: f32,
    /// Y position in color gamut
    pub y: f32,
}

#[derive(Debug)]
pub enum ParseColorError {
    InvalidByte,
    InvalidLength,
}

impl CIEColor {
    pub fn from_rgb(rgb: [u8; 3]) -> CIEColor {
        let r = rgb[0] as f32;
        let g = rgb[1] as f32;
        let b = rgb[2] as f32;
        let x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = 0.0193 * r + 0.1192 * g + 0.9505 * b;

        CIEColor {
            x: x / (x + y + z),
            y: y / (x + y + z),
        }
    }

    pub fn from_hex(hex: impl Into<String>) -> Result<CIEColor, ParseColorError> {
        let str: String = hex.into();
        let len = str.len();
        let is_shorthand = len == 3 || len == 4;
        let mut chars = str.chars();

        fn parse_char(c: char) -> Result<u8, ParseColorError> {
            match c {
                digit if c >= '0' && c <= '9' => Ok(digit as u8 - 48),
                upper if c >= 'A' && c <= 'F' => Ok(upper as u8 - 55),
                lower if c >= 'a' && c <= 'f' => Ok(lower as u8 - 87),
                _ => Err(ParseColorError::InvalidByte),
            }
        }

        if ![3, 4, 6, 7].contains(&len) {
            return Err(ParseColorError::InvalidLength);
        }
        if [4, 7].contains(&len) {
            if chars.next() != Some('#') {
                return Err(ParseColorError::InvalidByte);
            }
        }

        match chars.enumerate().try_fold([0u8, 0, 0], |mut acc, (i, c)| {
            if let Ok(b) = parse_char(c) {
                if is_shorthand {
                    acc[i] = b * 17;
                } else {
                    let idx = i / 2;
                    acc[idx] |= b << if i % 2 == 0 { 0 } else { 1 };
                }
                Some(acc)
            } else {
                None
            }
        }) {
            Some(rgb) => Ok(CIEColor::from_rgb(rgb)),
            None => Err(ParseColorError::InvalidByte),
        }
    }
}

/// The gammut types supported by hue
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum GamutType {
    /// Gamut of early Philips color-only products
    A,
    /// Limited gamut of first Hue color products
    B,
    /// Richer color gamut of Hue white and color ambiance products
    C,
    /// Color gamut of non-hue products with non-hue gamuts resp w/o gamut
    #[serde(rename = "other")]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DynamicsState {
    /// Current status of the lamp with dynamics.
    pub status: DynamicsStatus,
    /// Statuses in which a lamp could be when playing dynamics.
    pub status_values: HashSet<DynamicsStatus>,
    /// Speed of dynamic palette or effect.
    /// The speed is valid for the dynamic palette if the status is [DynamicsStatus::DynamicPalette] or for
    /// the corresponding effect listed in status. In case of status none, the speed is not valid.
    pub speed: f32,
    /// Indicates whether the value presented in speed is valid
    pub speed_valid: bool,
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DynamicsStatus {
    DynamicPalette,
    None,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlertState {
    /// Alert effects that the light supports.
    pub action_values: HashSet<AlertEffectType>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertEffectType {
    Breathe,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SignalingState {
    /// Signals that the light supports.
    pub signal_values: Option<HashSet<SignalType>>,
    /// Indicates status of active signal. Not available when inactive.
    pub status: Option<SignalStatus>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SignalStatus {
    /// Indicates which signal is currently active.
    pub signal: SignalType,
    /// Timestamp indicating when the active signal is expected to end. Value is not set if there is NoSignal.
    pub estimated_end: String,
    /// Colors that were provided for the active effect.
    pub colors: Vec<ColorFeatureBasic>,
}

#[derive(Copy, Clone, Eq, Debug, Deserialize, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    /// Stop active signal.
    NoSignal,
    /// Toggle between max brightness and off in fixed color.
    OnOff,
    /// Toggles between off and max brightness with a provided color.
    OnOffColor,
    /// Alternates between two provided colors.
    Alternating,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Normal,
    Streaming,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GradientState {
    /// Collection of gradients points.
    /// For control of the gradient points through a PUT a minimum of 2 points need to be provided.
    pub points: Vec<GradientPoint>,
    /// Mode in which the points are currently being deployed.
    /// If not provided during PUT/POST it will be defaulted to [GradientMode::InterpolatedPalette].
    pub mode: GradientMode,
    /// Modes a gradient device can deploy the gradient palette of colors.
    pub mode_values: HashSet<GradientMode>,
    /// Number of color points that gradient lamp is capable of showing with gradience.
    pub points_capable: usize,
    /// Number of pixels in the device
    pub pixel_count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GradientPoint {
    pub color: ColorFeatureBasic,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ColorFeatureBasic {
    pub xy: CIEColor,
}

impl ColorFeatureBasic {
    pub fn xy(x: f32, y: f32) -> Self {
        ColorFeatureBasic {
            xy: CIEColor { x, y },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GradientMode {
    InterpolatedPalette,
    InterpolatedPaletteMirrored,
    RandomPixelated,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EffectState {
    pub effect: Option<EffectType>,
    /// Possible effect values you can set in a light.
    pub effect_values: HashSet<EffectType>,
    /// Current status values the light is in regarding effects.
    pub status: EffectType,
    /// Possible status values in which a light could be when playing an effect.
    pub status_values: HashSet<EffectType>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectType {
    Prism,
    Opal,
    Glisten,
    Sparkle,
    Fire,
    Candle,
    NoEffect,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TimedEffectState {
    pub effect: Option<TimedEffectType>,
    /// Possible timed effect values you can set in a light.
    pub effect_values: HashSet<TimedEffectType>,
    /// Current status values the light is in regarding timed effects.
    pub status: TimedEffectType,
    /// Possible status values in which a light could be when playing a timed effect.
    pub status_values: HashSet<TimedEffectType>,
    /// Duration (ms) is mandatory when timed effect is set except for NoEffect.
    /// Resolution decreases for a larger duration. e.g effects with duration smaller than a minute
    /// will be rounded to a resolution of 1s, while effects with duration larger than an hour
    /// will be arounded up to a resolution of 300s. Duration has a max of 21600000 ms.
    pub duration: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimedEffectType {
    Sunrise,
    NoEffect,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PowerupState {
    /// When setting the [PowerupPresetType::Custom] preset the additional properties can be set.
    /// For all other presets, no other properties can be included.
    pub preset: PowerupPresetType,
    /// Indicates if the shown values have been configured in the lightsource.
    pub configured: bool,
    /// State to activate after powerup.
    pub on: PowerupOnState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerupPresetType {
    Safety,
    Powerfail,
    LastOnState,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PowerupOnState {
    /// State to activate after powerup. When setting mode [PowerupOnMode::On], the `on` property must be included.
    pub mode: PowerupOnMode,
    pub on: Option<OnState>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerupOnMode {
    /// Use the value specified in the [PowerupOnState] `on` property.
    On,
    /// Alternate between on and off on each subsequent power toggle.
    Toggle,
    /// Return to the state it was in before powering off.
    Previous,
}
