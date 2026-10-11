//! Shared test helpers for the STP primitive unit tests.

use alloc::collections::BTreeMap;

use crate::identity::NodeAddr;
use crate::proto::stp::{ParentDeclaration, TreeCoordinate, TreeState};

pub(super) fn make_node_addr(val: u8) -> NodeAddr {
    let mut bytes = [0u8; 16];
    bytes[0] = val;
    NodeAddr::from_bytes(bytes)
}

pub(super) fn make_coords(ids: &[u8]) -> TreeCoordinate {
    TreeCoordinate::from_addrs(ids.iter().map(|&v| make_node_addr(v)).collect()).unwrap()
}

/// Build a TreeState with our own coordinates set.
pub(super) fn make_tree_state(my_addr: u8, coord_path: &[u8]) -> TreeState {
    let my_node = make_node_addr(my_addr);
    let mut state = TreeState::new(my_node, 1000);
    let coords = make_coords(coord_path);
    state.root = *coords.root_id();
    state.my_coords = coords;
    state
}

/// Add a peer with given coordinates to the tree state.
pub(super) fn add_peer(state: &mut TreeState, peer_addr: u8, coord_path: &[u8]) {
    let peer = make_node_addr(peer_addr);
    let parent = make_node_addr(coord_path[1]);
    state.update_peer(
        ParentDeclaration::new(peer, parent, 1, 1000),
        make_coords(coord_path),
    );
}

/// Build a peer_costs map from (addr_byte, cost) pairs.
pub(super) fn make_costs(entries: &[(u8, f64)]) -> BTreeMap<NodeAddr, f64> {
    entries
        .iter()
        .map(|&(addr, cost)| (make_node_addr(addr), cost))
        .collect()
}

/// A random signing key, standing in for the root crate's `Identity`: the
/// codec tests need a declaration signed by the node it names.
pub(super) struct Signer {
    keypair: secp256k1::Keypair,
    node_addr: NodeAddr,
}

impl Signer {
    /// A signer with a fresh random key.
    pub(super) fn generate() -> Self {
        let keypair = crate::testutil::make_keypair();
        let node_addr = NodeAddr::from_pubkey(&keypair.x_only_public_key().0);
        Self { keypair, node_addr }
    }

    /// The node address derived from the signer's public key.
    pub(super) fn node_addr(&self) -> &NodeAddr {
        &self.node_addr
    }

    /// Sign `data` the way `Identity::sign` does: Schnorr over its SHA-256.
    pub(super) fn sign(&self, data: &[u8]) -> secp256k1::schnorr::Signature {
        let digest = crate::identity::sha256(data);
        crate::identity::secp().sign_schnorr(&digest, &self.keypair)
    }
}
