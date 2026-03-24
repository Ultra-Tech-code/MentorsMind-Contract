#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, Symbol, Vec, symbol_short};

#[allow(unused_imports)]
use soroban_sdk::token::TokenInterface as _;

// Escrow status enum
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Active,
    Released,
    Disputed,
    Refunded,
}

// Escrow data structure
#[contracttype]
#[derive(Clone, Debug)]
pub struct Escrow {
    pub id: u64,
    pub mentor: Address,
    pub learner: Address,
    pub amount: i128,
    pub session_id: Symbol,
    pub status: EscrowStatus,
    pub created_at: u64,
    pub token_address: Address,
}

const ESCROW_COUNT: Symbol = symbol_short!("ESC_CNT");
const ADMIN: Symbol = symbol_short!("ADMIN");
// FIX #18: key for the approved-token allowlist
const APPROVED_TOKENS: Symbol = symbol_short!("APR_TOKS");

// TTL constants
const ESCROW_TTL_THRESHOLD: u32 = 500_000;
const ESCROW_TTL_BUMP: u32 = 1_000_000;

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Initialize the contract with an admin.
    /// FIX #18: also accepts the list of approved token addresses
    /// (XLM native contract, USDC, PYUSD) so they can be validated later.
    pub fn initialize(env: Env, admin: Address, approved_tokens: Vec<Address>) {
        if env.storage().persistent().has(&ADMIN) {
            panic!("Already initialized");
        }

        env.storage().persistent().set(&ADMIN, &admin);
        env.storage().persistent().extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        // FIX #18: persist the allowlist
        env.storage().persistent().set(&APPROVED_TOKENS, &approved_tokens);
        env.storage().persistent().extend_ttl(&APPROVED_TOKENS, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.storage().persistent().set(&ESCROW_COUNT, &0u64);
        env.storage().persistent().extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
    }

    /// Create a new escrow.
    pub fn create_escrow(
        env: Env,
        mentor: Address,
        learner: Address,
        amount: i128,
        session_id: Symbol,
        token_address: Address,
    ) -> u64 {
        if amount <= 0 {
            panic!("Amount must be greater than zero");
        }

        learner.require_auth();

        // FIX #18: validate token is on the approved allowlist (XLM, USDC, PYUSD)
        env.storage().persistent().extend_ttl(&APPROVED_TOKENS, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        let approved: Vec<Address> = env
            .storage()
            .persistent()
            .get(&APPROVED_TOKENS)
            .expect("Approved tokens not set");
        let is_approved = (0..approved.len()).any(|i| approved.get_unchecked(i) == token_address);
        if !is_approved {
            panic!("Token not approved; must be XLM, USDC, or PYUSD");
        }

        // FIX #18: check learner balance before attempting transfer
        let token_client = token::Client::new(&env, &token_address);
        let balance = token_client.balance(&learner);
        if balance < amount {
            panic!("Insufficient token balance");
        }

        let mut count: u64 = env.storage().persistent().get(&ESCROW_COUNT).unwrap_or(0);
        count += 1;

        // BUG FIX: persist the updated count (was never saved, causing every escrow to get id=1)
        env.storage().persistent().set(&ESCROW_COUNT, &count);
        env.storage().persistent().extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        // Transfer tokens from learner to contract
        token_client.transfer(&learner, &env.current_contract_address(), &amount);

        let escrow = Escrow {
            id: count,
            mentor: mentor.clone(),
            learner: learner.clone(),
            amount,
            session_id: session_id.clone(),
            status: EscrowStatus::Active,
            created_at: env.ledger().timestamp(),
            token_address: token_address.clone(),
        };

        let key = (symbol_short!("ESCROW"), count);
        env.storage().persistent().set(&key, &escrow);
        env.storage().persistent().extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        env.events().publish(
            (symbol_short!("created"), count),
            (mentor, learner, amount, session_id, token_address), // token_address already present
        );

        count
    }

    /// Release funds to mentor (called by learner or admin).
    pub fn release_funds(env: Env, caller: Address, escrow_id: u64) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage().persistent().extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        let mut escrow: Escrow = env.storage().persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        let admin: Address = env.storage().persistent().get(&ADMIN).expect("Admin not found");
        env.storage().persistent().extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);

        caller.require_auth();
        if caller != escrow.learner && caller != admin {
            panic!("Caller not authorized");
        }

        let token_client = token::Client::new(&env, &escrow.token_address);
        token_client.transfer(&env.current_contract_address(), &escrow.mentor, &escrow.amount);

        escrow.status = EscrowStatus::Released;
        env.storage().persistent().set(&key, &escrow);

        // FIX #18: include token_address in the released event
        env.events().publish(
            (symbol_short!("released"), escrow_id),
            (escrow.mentor.clone(), escrow.amount, escrow.token_address.clone()),
        );
    }

    /// Open a dispute (called by mentor or learner).
    pub fn dispute(env: Env, caller: Address, escrow_id: u64) {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage().persistent().extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        let mut escrow: Escrow = env.storage().persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status != EscrowStatus::Active {
            panic!("Escrow not active");
        }

        caller.require_auth();
        if caller != escrow.mentor && caller != escrow.learner {
            panic!("Caller not authorized to dispute");
        }

        escrow.status = EscrowStatus::Disputed;
        env.storage().persistent().set(&key, &escrow);

        env.events().publish(
            (symbol_short!("disputed"), escrow_id),
            (escrow_id, escrow.token_address.clone()),
        );
    }

    /// Refund to learner (called by admin).
    pub fn refund(env: Env, escrow_id: u64) {
        let admin: Address = env.storage().persistent().get(&ADMIN).expect("Admin not found");
        env.storage().persistent().extend_ttl(&ADMIN, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        admin.require_auth();

        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage().persistent().extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        let mut escrow: Escrow = env.storage().persistent()
            .get(&key)
            .expect("Escrow not found");

        if escrow.status == EscrowStatus::Released || escrow.status == EscrowStatus::Refunded {
            panic!("Cannot refund");
        }

        let token_client = token::Client::new(&env, &escrow.token_address);
        token_client.transfer(&env.current_contract_address(), &escrow.learner, &escrow.amount);

        escrow.status = EscrowStatus::Refunded;
        env.storage().persistent().set(&key, &escrow);

        // FIX #18: include token_address in the refunded event
        env.events().publish(
            (symbol_short!("refunded"), escrow_id),
            (escrow.learner.clone(), escrow.amount, escrow.token_address.clone()),
        );
    }

    /// Get escrow details.
    pub fn get_escrow(env: Env, escrow_id: u64) -> Escrow {
        let key = (symbol_short!("ESCROW"), escrow_id);
        env.storage().persistent().extend_ttl(&key, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage().persistent()
            .get(&key)
            .expect("Escrow not found")
    }

    /// Get total escrow count.
    pub fn get_escrow_count(env: Env) -> u64 {
        env.storage().persistent().extend_ttl(&ESCROW_COUNT, ESCROW_TTL_THRESHOLD, ESCROW_TTL_BUMP);
        env.storage().persistent().get(&ESCROW_COUNT).unwrap_or(0)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::Address as _,
        token::StellarAssetClient,
        Address, Env, Vec, symbol_short,
    };

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Registers a real Stellar Asset Contract so that `balance` and `transfer`
    /// calls inside the escrow contract resolve against an actual contract.
    /// Returns the token address; the learner is pre-minted 10_000 tokens.
    fn setup_token(env: &Env, learner: &Address) -> Address {
        let token_admin = Address::generate(env);
        let sac = env.register_stellar_asset_contract_v2(token_admin.clone());
        let token_address = sac.address();
        // mint requires the token admin's auth — mock it before calling
        env.mock_all_auths();
        StellarAssetClient::new(env, &token_address).mint(learner, &10_000);
        token_address
    }

    fn setup_client(env: &Env) -> (EscrowContractClient<'_>, Address, Address, Address) {
        let contract_id = env.register_contract(None, EscrowContract);
        let client  = EscrowContractClient::new(env, &contract_id);
        let admin   = Address::generate(env);
        let mentor  = Address::generate(env);
        let learner = Address::generate(env);
        (client, admin, mentor, learner)
    }

    fn token_vec(env: &Env, token: &Address) -> Vec<Address> {
        let mut v = Vec::new(env);
        v.push_back(token.clone());
        v
    }

    // ── initialize ─────────────────────────────────────────────────────────────

    #[test]
    fn test_initialize_succeeds() {
        let env = Env::default();
        let (client, admin, _, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_initialize_reinit_rejected() {
        let env = Env::default();
        let (client, admin, _, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        let tokens = token_vec(&env, &token);
        client.initialize(&admin, &tokens);
        let other = Address::generate(&env);
        client.initialize(&other, &tokens);
    }

    // ── create_escrow ──────────────────────────────────────────────────────────

    #[test]
    fn test_create_escrow_valid() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id = client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &token);
        assert_eq!(id, 1);

        let escrow = client.get_escrow(&id);
        assert_eq!(escrow.amount, 1000);
        assert_eq!(escrow.status, EscrowStatus::Active);
        assert_eq!(escrow.token_address, token);
    }

    #[test]
    #[should_panic(expected = "Amount must be greater than zero")]
    fn test_create_escrow_zero_amount_rejected() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();
        client.create_escrow(&mentor, &learner, &0, &symbol_short!("S1"), &token);
    }

    #[test]
    #[should_panic(expected = "Token not approved")]
    fn test_create_escrow_unapproved_token_rejected() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();
        // Register a second SAC that is NOT on the allowlist
        let other_admin = Address::generate(&env);
        let bad_token = env
            .register_stellar_asset_contract_v2(other_admin)
            .address();
        client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &bad_token);
    }

    #[test]
    #[should_panic(expected = "Insufficient token balance")]
    fn test_create_escrow_insufficient_balance_rejected() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner); // mints 10_000
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();
        // Ask for more than the learner has
        client.create_escrow(&mentor, &learner, &99_999, &symbol_short!("S1"), &token);
    }

    // ── escrow count (regression for count-never-saved bug) ───────────────────

    #[test]
    fn test_escrow_count_increments_correctly() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id1 = client.create_escrow(&mentor, &learner, &100, &symbol_short!("S1"), &token);
        let id2 = client.create_escrow(&mentor, &learner, &200, &symbol_short!("S2"), &token);
        let id3 = client.create_escrow(&mentor, &learner, &300, &symbol_short!("S3"), &token);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(client.get_escrow_count(), 3);
    }

    // ── release_funds ──────────────────────────────────────────────────────────

    #[test]
    fn test_release_funds_by_learner() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id = client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &token);
        client.release_funds(&learner, &id);

        assert_eq!(client.get_escrow(&id).status, EscrowStatus::Released);
    }

    #[test]
    #[should_panic(expected = "Caller not authorized")]
    fn test_release_funds_unauthorized_caller_rejected() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id = client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &token);
        let unauthorized = Address::generate(&env);
        client.release_funds(&unauthorized, &id);
    }

    // ── dispute ────────────────────────────────────────────────────────────────

    #[test]
    fn test_dispute_by_mentor_and_learner() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id1 = client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &token);
        client.dispute(&mentor, &id1);
        assert_eq!(client.get_escrow(&id1).status, EscrowStatus::Disputed);

        let id2 = client.create_escrow(&mentor, &learner, &2000, &symbol_short!("S2"), &token);
        client.dispute(&learner, &id2);
        assert_eq!(client.get_escrow(&id2).status, EscrowStatus::Disputed);
    }

    #[test]
    #[should_panic(expected = "Caller not authorized to dispute")]
    fn test_dispute_unauthorized_caller_rejected() {
        let env = Env::default();
        let (client, admin, mentor, learner) = setup_client(&env);
        let token = setup_token(&env, &learner);
        client.initialize(&admin, &token_vec(&env, &token));
        env.mock_all_auths();

        let id = client.create_escrow(&mentor, &learner, &1000, &symbol_short!("S1"), &token);
        let random = Address::generate(&env);
        client.dispute(&random, &id);
    }
}