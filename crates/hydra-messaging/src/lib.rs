#![forbid(unsafe_code)]
//! Persona-bound Nostr inbox policy and message-request classification.

use hydra_domain::{NostrPublicKey, PersonaId, PrivateState};
use hydra_store::DurableStore;

#[must_use]
pub fn is_message_request(
    store: &DurableStore,
    private: &PrivateState,
    persona: PersonaId,
    peer: &NostrPublicKey,
    outgoing: bool,
) -> bool {
    if outgoing {
        return false;
    }
    let publicly_followed = store
        .state()
        .follows
        .get(&(persona, peer.clone()))
        .is_some_and(|follow| follow.following);
    let privately_followed = private
        .follows
        .get(peer)
        .is_some_and(|follow| follow.following);
    !(publicly_followed || privately_followed)
}
