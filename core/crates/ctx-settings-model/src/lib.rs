mod defaults;
mod model;
mod public;
pub mod update;

pub use defaults::ensure_settings_defaults;
pub use model::*;
pub use public::*;
pub use update::*;

pub fn to_public(settings: &Settings) -> PublicSettings {
    public::to_public(settings)
}

pub fn apply_update(current: Settings, req: UpdateSettingsReq) -> Settings {
    let mut next = update::apply_update(current, req);
    normalize_settings_in_place(&mut next);
    next
}
