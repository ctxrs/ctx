use super::*;

mod connection_profiles;
mod devices;
mod types;

use types::{CreateMobileConnectionProfileReq, CreateMobileConnectionProfileResp};

pub(in crate::api) use connection_profiles::{
    create_mobile_connection_profile, delete_mobile_connection_profile,
    list_mobile_connection_profiles,
};
pub(in crate::api) use devices::{list_mobile_devices_for_profile, register_mobile_device};
