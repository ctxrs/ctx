#[derive(rkyv::Archive, rkyv::Serialize)]
#[allow(dead_code)] // The owned form is constructed by build.rs; runtime borrows ArchivedData.
pub struct Data {
    pub lengths: Vec<u32>,
    pub splits: Vec<[u32; 2]>,
    pub prefix: Vec<u32>,
    pub pairs: std::collections::HashMap<
        u64,
        u32,
        std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>,
    >,
    pub states: Vec<[u32; 3]>,
    pub outputs: Vec<u32>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn safe_archive_access_rejects_invalid_roots() {
        let mut bytes = rkyv::util::AlignedVec::<16>::new();
        bytes.extend_from_slice(include_bytes!(concat!(env!("OUT_DIR"), "/count.rkyv")));
        assert!(rkyv::access::<super::ArchivedData, rkyv::rancor::Error>(&bytes).is_ok());
        assert!(rkyv::access::<super::ArchivedData, rkyv::rancor::Error>(&bytes[..8]).is_err());
        let root = bytes.len() - std::mem::size_of::<super::ArchivedData>();
        bytes[root..].fill(255);
        assert!(rkyv::access::<super::ArchivedData, rkyv::rancor::Error>(&bytes).is_err());
    }
    #[test]
    fn safe_dfa_access_rejects_invalid_data() {
        let mut bytes = rkyv::util::AlignedVec::<16>::new();
        bytes.extend_from_slice(include_bytes!(concat!(env!("OUT_DIR"), "/pre.dense")));
        assert!(regex_automata::dfa::dense::DFA::from_bytes(&bytes).is_ok());
        assert!(regex_automata::dfa::dense::DFA::from_bytes(&bytes[..8]).is_err());
        bytes[0] ^= 255;
        assert!(regex_automata::dfa::dense::DFA::from_bytes(&bytes).is_err());
    }
}
