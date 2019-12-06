use solana_client::thin_client::ThinClient;
use solana_core::contact_info::ContactInfo;
use solana_core::validator::Validator;
use solana_core::validator::ValidatorConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ValidatorInfo {
    pub keypair: Arc<Keypair>,
    pub voting_keypair: Arc<Keypair>,
    pub storage_keypair: Arc<Keypair>,
    pub ledger_path: PathBuf,
    pub contact_info: ContactInfo,
}

pub struct ClusterValidatorInfo {
    pub info: ValidatorInfo,
    pub config: ValidatorConfig,
    pub validator: Option<Validator>,
}

impl ClusterValidatorInfo {
    pub fn new(
        validator_info: ValidatorInfo,
        config: ValidatorConfig,
        validator: Validator,
    ) -> Self {
        Self {
            info: validator_info,
            config,
            validator: Some(validator),
        }
    }
}

pub trait Cluster {
    fn get_node_pubkeys(&self) -> Vec<Pubkey>;
    fn get_validator_client(&self, pubkey: &Pubkey) -> Option<ThinClient>;
    fn exit_node(&mut self, pubkey: &Pubkey) -> ClusterValidatorInfo;
    fn restart_node(&mut self, pubkey: &Pubkey, cluster_validator_info: ClusterValidatorInfo);
    fn exit_restart_node(&mut self, pubkey: &Pubkey, config: ValidatorConfig);
}