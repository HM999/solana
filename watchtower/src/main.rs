//! A command-line executable for monitoring the health of a cluster

mod notifier;

use crate::notifier::Notifier;
use clap::{crate_description, crate_name, value_t, value_t_or_exit, App, Arg};
use log::*;
use solana_clap_utils::{
    input_parsers::pubkeys_of,
    input_validators::{is_pubkey_or_keypair, is_url},
};
use solana_client::{
    client_error::Result as ClientResult, rpc_client::RpcClient, rpc_response::RpcVoteAccountStatus,
};
use solana_metrics::{datapoint_error, datapoint_info};
use solana_sdk::{hash::Hash, native_token::lamports_to_sol, pubkey::Pubkey};
use std::{error, str::FromStr, thread::sleep, time::Duration};

struct Config {
    interval: Duration,
    json_rpc_url: String,
    validator_identity_pubkeys: Vec<String>,
    no_duplicate_notifications: bool,
    monitor_active_stake: bool,
}

fn get_config() -> Config {
    let matches = App::new(crate_name!())
        .about(crate_description!())
        .version(solana_clap_utils::version!())
        .arg({
            let arg = Arg::with_name("config_file")
                .short("C")
                .long("config")
                .value_name("PATH")
                .takes_value(true)
                .global(true)
                .help("Configuration file to use");
            if let Some(ref config_file) = *solana_cli_config::CONFIG_FILE {
                arg.default_value(&config_file)
            } else {
                arg
            }
        })
        .arg(
            Arg::with_name("json_rpc_url")
                .long("url")
                .value_name("URL")
                .takes_value(true)
                .validator(is_url)
                .help("JSON RPC URL for the cluster"),
        )
        .arg(
            Arg::with_name("interval")
                .long("interval")
                .value_name("SECONDS")
                .takes_value(true)
                .default_value("60")
                .help("Wait interval seconds between checking the cluster"),
        )
        .arg(
            Arg::with_name("validator_identities")
                .long("validator-identity")
                .value_name("VALIDATOR IDENTITY PUBKEY")
                .takes_value(true)
                .validator(is_pubkey_or_keypair)
                .multiple(true)
                .help("Monitor a specific validator only instead of the entire cluster"),
        )
        .arg(
            Arg::with_name("no_duplicate_notifications")
                .long("no-duplicate-notifications")
                .takes_value(false)
                .help("Subsequent identical notifications will be suppressed"),
        )
        .arg(
            Arg::with_name("monitor_active_stake")
                .long("monitor-active-stake")
                .takes_value(false)
                .help("Alert when the current stake for the cluster drops below 80%"),
        )
        .get_matches();

    let config = if let Some(config_file) = matches.value_of("config_file") {
        solana_cli_config::Config::load(config_file).unwrap_or_default()
    } else {
        solana_cli_config::Config::default()
    };

    let interval = Duration::from_secs(value_t_or_exit!(matches, "interval", u64));
    let json_rpc_url =
        value_t!(matches, "json_rpc_url", String).unwrap_or_else(|_| config.json_rpc_url);
    let validator_identity_pubkeys: Vec<_> = pubkeys_of(&matches, "validator_identities")
        .unwrap_or_else(|| vec![])
        .into_iter()
        .map(|i| i.to_string())
        .collect();

    let no_duplicate_notifications = matches.is_present("no_duplicate_notifications");
    let monitor_active_stake = matches.is_present("monitor_active_stake");

    let config = Config {
        interval,
        json_rpc_url,
        validator_identity_pubkeys,
        no_duplicate_notifications,
        monitor_active_stake,
    };

    info!("RPC URL: {}", config.json_rpc_url);
    if !config.validator_identity_pubkeys.is_empty() {
        info!(
            "Monitored validators: {:?}",
            config.validator_identity_pubkeys
        );
    }
    config
}

fn get_cluster_info(rpc_client: &RpcClient) -> ClientResult<(u64, Hash, RpcVoteAccountStatus)> {
    let transaction_count = rpc_client.get_transaction_count()?;
    let recent_blockhash = rpc_client.get_recent_blockhash()?.0;
    let vote_accounts = rpc_client.get_vote_accounts()?;
    Ok((transaction_count, recent_blockhash, vote_accounts))
}

fn main() -> Result<(), Box<dyn error::Error>> {
    let config = get_config();

    solana_logger::setup_with_default("solana=info");
    solana_metrics::set_panic_hook("watchtower");

    let rpc_client = RpcClient::new(config.json_rpc_url);

    let notifier = Notifier::new();
    let mut last_transaction_count = 0;
    let mut last_recent_blockhash = Hash::default();
    let mut last_notification_msg = "".into();

    loop {
        let failure = match get_cluster_info(&rpc_client) {
            Ok((transaction_count, recent_blockhash, vote_accounts)) => {
                info!("Current transaction count: {}", transaction_count);
                info!("Recent blockhash: {}", recent_blockhash);
                info!("Current validator count: {}", vote_accounts.current.len());
                info!(
                    "Delinquent validator count: {}",
                    vote_accounts.delinquent.len()
                );

                let mut failures = vec![];

                let total_current_stake = vote_accounts
                    .current
                    .iter()
                    .fold(0, |acc, vote_account| acc + vote_account.activated_stake);
                let total_delinquent_stake = vote_accounts
                    .delinquent
                    .iter()
                    .fold(0, |acc, vote_account| acc + vote_account.activated_stake);

                let total_stake = total_current_stake + total_delinquent_stake;
                let current_stake_percent = total_current_stake * 100 / total_stake;
                info!(
                    "Current stake: {}% | Total stake: {} SOL, current stake: {} SOL, delinquent: {} SOL",
                    current_stake_percent,
                    lamports_to_sol(total_stake),
                    lamports_to_sol(total_current_stake),
                    lamports_to_sol(total_delinquent_stake)
                );

                if transaction_count > last_transaction_count {
                    last_transaction_count = transaction_count;
                } else {
                    failures.push((
                        "transaction-count",
                        format!(
                            "Transaction count is not advancing: {} <= {}",
                            transaction_count, last_transaction_count
                        ),
                    ));
                }

                if recent_blockhash != last_recent_blockhash {
                    last_recent_blockhash = recent_blockhash;
                } else {
                    failures.push((
                        "recent-blockhash",
                        format!("Unable to get new blockhash: {}", recent_blockhash),
                    ));
                }

                if config.monitor_active_stake && current_stake_percent < 80 {
                    failures.push((
                        "current-stake",
                        format!("Current stake is {}%", current_stake_percent),
                    ));
                }

                if config.validator_identity_pubkeys.is_empty() {
                    if !vote_accounts.delinquent.is_empty() {
                        failures.push((
                            "delinquent",
                            format!("{} delinquent validators", vote_accounts.delinquent.len()),
                        ));
                    }
                } else {
                    let mut errors = vec![];
                    for validator_identity in config.validator_identity_pubkeys.iter() {
                        if vote_accounts
                            .delinquent
                            .iter()
                            .any(|vai| vai.node_pubkey == *validator_identity)
                        {
                            errors.push(format!("{} delinquent", validator_identity));
                        } else if !vote_accounts
                            .current
                            .iter()
                            .any(|vai| vai.node_pubkey == *validator_identity)
                        {
                            errors.push(format!("{} missing", validator_identity));
                        }

                        rpc_client
                            .get_balance(&Pubkey::from_str(&validator_identity).unwrap_or_default())
                            .map(lamports_to_sol)
                            .map(|balance| {
                                if balance < 1.0 {
                                    failures.push((
                                        "balance",
                                        format!("{} has {} SOL", validator_identity, balance),
                                    ));
                                }
                            })
                            .unwrap_or_else(|err| {
                                warn!("Failed to get balance of {}: {:?}", validator_identity, err);
                            });
                    }

                    if !errors.is_empty() {
                        failures.push(("delinquent", errors.join(",")));
                    }
                }

                for failure in failures.iter() {
                    error!("{} sanity failure: {}", failure.0, failure.1);
                }
                failures.into_iter().next() // Only report the first failure if any
            }
            Err(err) => Some(("rpc", err.to_string())),
        };

        datapoint_info!("watchtower-sanity", ("ok", failure.is_none(), bool));
        if let Some((failure_test_name, failure_error_message)) = &failure {
            let notification_msg = format!(
                "solana-watchtower: Error: {}: {}",
                failure_test_name, failure_error_message
            );
            if !config.no_duplicate_notifications || last_notification_msg != notification_msg {
                notifier.send(&notification_msg);
            }
            datapoint_error!(
                "watchtower-sanity-failure",
                ("test", failure_test_name, String),
                ("err", failure_error_message, String)
            );
            last_notification_msg = notification_msg;
        } else {
            if !last_notification_msg.is_empty() {
                info!("All clear");
                notifier.send("solana-watchtower: All clear");
            }
            last_notification_msg = "".into();
        }
        sleep(config.interval);
    }
}
