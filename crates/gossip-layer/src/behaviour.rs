//! Custom network behaviour for the gossip mesh.

use libp2p::gossipsub;
use libp2p::swarm::NetworkBehaviour;

/// Combined network behaviour for the mesh network.
///
/// This includes gossipsub for epidemic broadcasts and can be extended
/// with additional protocols like Kademlia for DHT-based peer discovery.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "GossipBehaviourEvent")]
pub struct GossipBehaviour {
    /// Gossipsub protocol for pub/sub
    pub gossipsub: gossipsub::Behaviour,
    // Future: Add Kademlia, mDNS, etc.
}

/// Events emitted by the gossip behaviour
#[derive(Debug)]
pub enum GossipBehaviourEvent {
    /// Gossipsub event
    Gossipsub(gossipsub::Event),
}

impl From<gossipsub::Event> for GossipBehaviourEvent {
    fn from(event: gossipsub::Event) -> Self {
        GossipBehaviourEvent::Gossipsub(event)
    }
}
