//! Phase 51 smoke test — end-to-end encrypted collaboration.
//!
//! Drives the whole shape a real session takes, rather than the unit
//! tests' individual properties:
//!
//! 1. An owner mints an admin key and a roster, and adds two people.
//! 2. Verification: both sides derive the same SAS from each other's
//!    public keys, and a substituted key produces a different code.
//! 3. A shared file encrypts once and every member decrypts it; a
//!    non-member with a perfectly valid key cannot.
//! 4. The roster survives a save/load round trip with its signatures
//!    intact, and a tampered roster is refused on load.
//! 5. Revoking a member locks them out of everything encrypted after —
//!    and, honestly, not out of what they already had.

use freally_collab::{
    AdminKey, CollabError, Roster, decrypt_with, encrypt_for, generate_member, sas,
};

#[test]
fn phase_51_collaboration_round_trip() {
    // 1. Owner sets up.
    let admin = AdminKey::generate().expect("admin key");
    let mut roster = Roster::new(&admin);

    let (alice_secret, alice_pub) = generate_member();
    let (bob_secret, bob_pub) = generate_member();
    roster.add_member("alice", &alice_pub).expect("add alice");
    roster.add_member("bob", &bob_pub).expect("add bob");
    assert_eq!(roster.members.len(), 2);

    // 2. Out-of-band verification. Both sides compute the same code
    //    without agreeing an order first.
    let code_from_alice = sas(&alice_pub, &bob_pub);
    let code_from_bob = sas(&bob_pub, &alice_pub);
    assert_eq!(code_from_alice, code_from_bob);

    // A machine-in-the-middle substituting its own key cannot make the
    // codes match — that is the whole point of comparing them aloud.
    let (_, attacker_pub) = generate_member();
    assert_ne!(sas(&alice_pub, &attacker_pub), code_from_alice);

    // 3. One encryption, every member reads it.
    let secret_doc = b"the quarterly numbers";
    let sealed = encrypt_for(&roster, secret_doc).expect("encrypt");
    assert_ne!(
        &sealed[..],
        &secret_doc[..],
        "the payload must not be stored in the clear"
    );
    assert_eq!(decrypt_with(&alice_secret, &sealed).unwrap(), secret_doc);
    assert_eq!(decrypt_with(&bob_secret, &sealed).unwrap(), secret_doc);

    let (outsider_secret, _) = generate_member();
    assert!(
        decrypt_with(&outsider_secret, &sealed).is_err(),
        "a valid key that is not on the roster must not decrypt"
    );

    // 4. Persistence round trip, signatures intact.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("collab-roster.json");
    std::fs::write(&path, roster.to_json().expect("serialise")).expect("write");

    let loaded = Roster::from_json(&std::fs::read_to_string(&path).unwrap()).expect("load");
    assert_eq!(loaded, roster);
    loaded.verify_revocations().expect("signatures verify");

    // 5. Revocation, and its honest limit.
    let mut roster = loaded;
    let epoch_before = roster.epoch;
    let token = roster.remove_member("alice", &admin).expect("revoke");
    assert!(roster.epoch > epoch_before, "a removal advances the epoch");
    assert_eq!(token.label, "alice");
    roster.verify_revocations().expect("revocation is signed");

    let after = encrypt_for(&roster, b"post-revocation").expect("encrypt");
    assert!(
        decrypt_with(&alice_secret, &after).is_err(),
        "a revoked member must not read anything encrypted afterwards"
    );
    assert_eq!(
        decrypt_with(&bob_secret, &after).unwrap(),
        b"post-revocation"
    );
    // The documented limit, asserted so it can never be quietly claimed
    // otherwise: bytes she already had remain readable.
    assert_eq!(decrypt_with(&alice_secret, &sealed).unwrap(), secret_doc);

    // A tampered roster is refused rather than silently trusted.
    let mut forged = roster.clone();
    forged.revocations[0].label = "bob".to_string();
    assert!(matches!(
        forged.verify_revocations(),
        Err(CollabError::BadRevocation { .. })
    ));
}

#[test]
fn phase_51_roster_rejects_unusable_input() {
    let admin = AdminKey::generate().expect("admin key");
    let mut roster = Roster::new(&admin);

    // A typo'd recipient must fail loudly at add time — accepting it
    // would make every future file silently unreadable by that person.
    assert!(matches!(
        roster.add_member("typo", "age1nonsense"),
        Err(CollabError::InvalidRecipient(_))
    ));

    // An empty roster cannot encrypt: producing a file nobody can open
    // would be worse than an error.
    assert!(matches!(
        encrypt_for(&roster, b"x"),
        Err(CollabError::EmptyRoster)
    ));

    // The last member cannot be removed — the roster would be dead.
    let (_, only_pub) = generate_member();
    roster.add_member("only", &only_pub).expect("add");
    assert!(matches!(
        roster.remove_member("only", &admin),
        Err(CollabError::LastMember)
    ));
}
