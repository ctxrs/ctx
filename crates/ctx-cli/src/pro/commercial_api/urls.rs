use anyhow::{bail, Result};
use url::Url;

use super::validate_https_url;

const STRIPE_CHECKOUT_HOST: &str = "checkout.stripe.com";
const STRIPE_PORTAL_HOST: &str = "billing.stripe.com";

pub(super) fn validate_checkout_url(value: &str) -> Result<Url> {
    validate_stripe_hosted_url(value, "Checkout", STRIPE_CHECKOUT_HOST)
}

pub(super) fn validate_portal_url(value: &str) -> Result<Url> {
    validate_stripe_hosted_url(value, "billing portal", STRIPE_PORTAL_HOST)
}

fn validate_stripe_hosted_url(value: &str, label: &str, expected_host: &str) -> Result<Url> {
    let parsed = validate_https_url(value, label)?;
    if parsed.host_str() != Some(expected_host) || parsed.port().is_some() {
        bail!("invalid_response: {label} URL is not Stripe-hosted");
    }
    Ok(parsed)
}
