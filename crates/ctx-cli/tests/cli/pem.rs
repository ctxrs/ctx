#[allow(unused_imports)]
use super::*;

pub(crate) fn pem_der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    BASE64.decode(body).unwrap()
}
