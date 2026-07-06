#[allow(unused_imports)]
use super::*;

pub(crate) fn sign_test_release_metadata(bytes: &[u8]) -> String {
    let key_pair = RsaKeyPair::from_pkcs8(&pem_der(TEST_RELEASE_PRIVATE_KEY_PEM)).unwrap();
    let rng = SystemRandom::new();
    let mut signature = vec![0; key_pair.public().modulus_len()];
    key_pair
        .sign(&RSA_PKCS1_SHA256, &rng, bytes, &mut signature)
        .unwrap();
    BASE64.encode(signature)
}
