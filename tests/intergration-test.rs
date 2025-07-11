use std::str::FromStr;

use soffer::{Offer, OfferStatus, OfferType, Processor, SwapInstruction};
// We need these tools to build our mini-playground and play with our smart contract.
use borsh::BorshDeserialize;
use solana_program::instruction::InstructionError;
use solana_program::{
    account_info::AccountInfo,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
    system_program,
    sysvar::{Sysvar, rent::Rent}, // For account rent calculations
};
use solana_program_test::{BanksClient, ProgramTest, ProgramTestContext, processor}; // Our mini-playground tools!
use solana_sdk::transaction::TransactionError;
use solana_sdk::{
    signature::{Keypair, Signer}, // To create new "people" (keypairs)
    transaction::Transaction,     // To bundle instructions into a transaction
};
use spl_token::state::{Account as TokenAccount, Mint}; // For SPL token accounts and mints

// Local msg! macro for logging in tests
macro_rules! msg {
    ($($arg:tt)*) => (println!($($arg)*));
}

use borsh::to_vec;
use soffer::SwapError;
use solana_program::program_error::ProgramError;

// Helper to fund an account with lamports
async fn fund_account(context: &mut (BanksClient, Keypair, Hash), pubkey: &Pubkey, lamports: u64) {
    let transfer_ix = system_instruction::transfer(&context.1.pubkey(), pubkey, lamports);
    let mut transaction = Transaction::new_with_payer(&[transfer_ix], Some(&context.1.pubkey()));
    transaction.sign(&[&context.1], context.2);
    context.0.process_transaction(transaction).await.unwrap();
}

async fn create_mint(
    context: &mut (BanksClient, Keypair, Hash),
    mint_authority: &Keypair,
    freeze_authority: Option<&Pubkey>,
    decimals: u8,
) -> Pubkey {
    let mint_keypair = Keypair::new(); // A new unique ID for our token blueprint
    let rent = context.0.get_rent().await.unwrap(); // Get rent info
    let rent_lamports = rent.minimum_balance(Mint::LEN); // How much SOL for the mint account

    // Create the mint account on our mini-playground.
    let create_mint_account_ix = system_instruction::create_account(
        &context.1.pubkey(),    // Who pays for the account
        &mint_keypair.pubkey(), // The new mint account's address
        rent_lamports,          // Rent amount
        Mint::LEN as u64,       // Size of the account
        &spl_token::id(),       // Owner of the account (SPL Token program)
    );

    // Initialize the mint (set up its rules, like who can create new tokens).
    let init_mint_ix = spl_token::instruction::initialize_mint(
        &spl_token::id(),         // SPL Token program ID
        &mint_keypair.pubkey(),   // Our new mint account
        &mint_authority.pubkey(), // Who can create new tokens
        freeze_authority,         // Who can freeze tokens (optional)
        decimals,                 // How many decimal places our token has
    )
    .unwrap();

    // Bundle these instructions into a transaction and send it.
    let mut transaction = Transaction::new_with_payer(
        &[create_mint_account_ix, init_mint_ix],
        Some(&context.1.pubkey()),
    );
    transaction.sign(&[&context.1, &mint_keypair], context.2);
    context.0.process_transaction(transaction).await.unwrap();

    mint_keypair.pubkey() // Return the address of our new token blueprint
}

async fn create_token_account(
    context: &mut (BanksClient, Keypair, Hash),
    owner: &Keypair,
    mint: &Pubkey,
) -> Pubkey {
    let token_account_keypair = Keypair::new(); // A new unique ID for our token wallet
    let rent = context.0.get_rent().await.unwrap();
    let rent_lamports = rent.minimum_balance(TokenAccount::LEN);

    // Create the token account.
    let create_token_account_ix = system_instruction::create_account(
        &context.1.pubkey(),
        &token_account_keypair.pubkey(),
        rent_lamports,
        TokenAccount::LEN as u64,
        &spl_token::id(),
    );

    // Initialize the token account (link it to a specific token blueprint and owner).
    let init_token_account_ix = spl_token::instruction::initialize_account(
        &spl_token::id(),
        &token_account_keypair.pubkey(),
        mint,
        &owner.pubkey(),
    )
    .unwrap();

    // Bundle and send the transaction.
    let mut transaction = Transaction::new_with_payer(
        &[create_token_account_ix, init_token_account_ix],
        Some(&context.1.pubkey()),
    );
    transaction.sign(&[&context.1, &token_account_keypair], context.2);
    context.0.process_transaction(transaction).await.unwrap();

    token_account_keypair.pubkey() // Return the address of our new token wallet
}
// This is like printing new trading cards and putting them in a wallet.
async fn mint_to(
    context: &mut (BanksClient, Keypair, Hash),
    mint: &Pubkey,
    token_account: &Pubkey,
    mint_authority: &Keypair,
    amount: u64,
) {
    let mint_to_ix = spl_token::instruction::mint_to(
        &spl_token::id(),
        mint,
        token_account,
        &mint_authority.pubkey(),
        &[],
        amount, // No multi-signers needed here
    )
    .unwrap();

    let mut transaction = Transaction::new_with_payer(&[mint_to_ix], Some(&context.1.pubkey()));
    transaction.sign(&[&context.1, mint_authority], context.2);
    context.0.process_transaction(transaction).await.unwrap();
}
async fn get_sol_balance(context: &mut (BanksClient, Keypair, Hash), pubkey: &Pubkey) -> u64 {
    context.0.get_balance(*pubkey).await.unwrap()
}

async fn get_token_balance(
    context: &mut (BanksClient, Keypair, Hash),
    token_account: &Pubkey,
) -> u64 {
    let account = context
        .0
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    let token_account_data = TokenAccount::unpack(&account.data).unwrap();
    token_account_data.amount
}

#[tokio::test]
async fn test_create_public_sell_offer_success() {
    let mut program_test = ProgramTest::new(
        "SOFFER", // Our program's name
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(), // Our program's ID (address)
        processor!(Processor::process), // Tell it to use our Processor,
    );
    // Give our payer (the person who pays for transactions) some SOL.
    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000, // 100 SOL
            ..Default::default()
        },
    );

    let mut context = program_test.start().await;
    let payer_pubkey = context.1.pubkey();
    fund_account(&mut context, &payer_pubkey, 100_000_000_000).await;
    // 2. Create some "people" and "tokens" for our test.
    let maker = Keypair::new(); // The person who will make the offer
    let mint_authority = Keypair::new(); // The person who can create new tokens
    // Fund the maker with SOL for rent and fees
    fund_account(&mut context, &maker.pubkey(), 10_000_000_000).await;
    let offer_token_mint = create_mint(&mut context, &mint_authority, None, 0).await; // Our shiny token type
    let receive_token_mint = create_mint(&mut context, &mint_authority, None, 0).await; // The token we want in return

    // Create a token account for the maker and give them some tokens.
    let maker_offer_token_account =
        create_token_account(&mut context, &maker, &offer_token_mint).await;
    // Fund the maker's token account with tokens
    mint_to(
        &mut context,
        &offer_token_mint,
        &maker_offer_token_account,
        &mint_authority,
        100,
    )
    .await;

    // Get the maker's SOL account (for rent and potentially receiving SOL).
    let maker_sol_account = context.1.pubkey(); // Use payer's account for SOL

    // 3. Prepare the instruction to create a public sell offer.
    // Maker offers 10 tokens for 5 SOL.
    let offer_token_amount = 10;
    let receive_token_amount = 5; // This represents 5 SOL in this case
    let offer_type = OfferType::PublicSell;

    // Calculate the PDA for the offer account.
    let (offer_account_pubkey, bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            offer_token_mint.as_ref(),
            receive_token_mint.as_ref(),
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let instruction_data = SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount,
        receive_token_amount,
        expiration: None, // No expiration for this test
        bump_seed,
    };

    let borsh_instruction_data = borsh::to_vec(&instruction_data).unwrap();

    let accounts = vec![
        AccountMeta::new(maker.pubkey(), true), // maker_account (signer)
        AccountMeta::new(offer_account_pubkey, false), // offer_account (writable, not signer, PDA)
        AccountMeta::new(maker_offer_token_account, false), // maker_token_account (writable)
        AccountMeta::new_readonly(offer_token_mint, false), // offer_token_mint
        AccountMeta::new_readonly(receive_token_mint, false), // receive_token_mint (representing SOL in this case)
        AccountMeta::new_readonly(system_program::id(), false), // system_program
        AccountMeta::new_readonly(spl_token::id(), false),    // token_program
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false), // rent_sysvar
        AccountMeta::new(maker_sol_account, false), // maker_sol_account (writable, for rent or future SOL transfers)
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts,
        data: borsh_instruction_data,
    };

    // 4. Send the transaction and check the result.
    let mut transaction = Transaction::new_with_payer(
        &[create_offer_ix],
        Some(&context.1.pubkey()), // Payer pays for transaction fees
    );
    // Only sign with payer and maker (maker is the only signer in the instruction)
    transaction.sign(&[&context.1, &maker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // 5. Verify the offer account was created and contains correct data.
    let offer_account = context
        .0
        .get_account(offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        offer_account.owner,
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap()
    ); // Check if our program owns it

    let offer_data = Offer::try_from_slice(&offer_account.data).unwrap(); // Unpack the data
    assert_eq!(offer_data.offer_type, OfferType::PublicSell);
    assert_eq!(offer_data.status, OfferStatus::Active);
    assert_eq!(offer_data.maker, maker.pubkey());
    assert_eq!(offer_data.offer_token_mint, offer_token_mint);
    assert_eq!(offer_data.offer_token_amount, offer_token_amount);
    assert_eq!(offer_data.receive_token_mint, receive_token_mint);
    assert_eq!(offer_data.receive_token_amount, receive_token_amount);
    assert_eq!(offer_data.escrow_sol_amount, 0); // No SOL escrowed for a sell offer
    assert_eq!(
        get_token_balance(&mut context, &maker_offer_token_account).await,
        100
    ); // Tokens still in maker's account until accepted
    msg!("test_create_public_sell_offer_success PASSED");
}

#[tokio::test]
async fn test_accept_public_sell_offer_success() {
    let mut program_test = ProgramTest::new(
        "soffer",
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        processor!(Processor::process),
    );

    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            ..Default::default()
        },
    );

    let mut context = program_test.start().await;

    let maker = Keypair::new();
    let taker = Keypair::new();
    let mint_authority = Keypair::new();
    let offer_token_mint = create_mint(&mut context, &mint_authority, None, 0).await; // Maker offers this token
    let receive_token_mint = Pubkey::new_from_array([0; 32]); // Taker offers SOL (represented by dummy Pubkey)

    // Maker's accounts
    let maker_offer_token_account =
        create_token_account(&mut context, &maker, &offer_token_mint).await;
    mint_to(
        &mut context,
        &offer_token_mint,
        &maker_offer_token_account,
        &mint_authority,
        100,
    )
    .await;
    let maker_sol_account = context.1.pubkey(); // Use payer's account for SOL

    // Taker's accounts
    let taker_receive_token_account =
        create_token_account(&mut context, &taker, &offer_token_mint).await; // Taker will receive this token
    let taker_sol_account = context.1.pubkey(); // Use payer's account for SOL

    // Create the offer (Maker sells 10 tokens for 5 SOL)
    let offer_token_amount = 10;
    let receive_sol_amount = 5_000_000_000; // 5 SOL
    let offer_type = OfferType::PublicSell;

    let (offer_account_pubkey, bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            offer_token_mint.as_ref(), // SOL placeholder
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let create_offer_in_data = SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount,
        receive_token_amount: receive_sol_amount,
        expiration: None,
        bump_seed,
    };
    let borsh_create_offer_in_data = borsh::to_vec(&create_offer_in_data).unwrap();

    let create_offer_accounts = vec![
        AccountMeta::new(maker.pubkey(), true),
        AccountMeta::new(offer_account_pubkey, false),
        AccountMeta::new(maker_offer_token_account, false),
        AccountMeta::new_readonly(offer_token_mint, false),
        AccountMeta::new_readonly(receive_token_mint, false), // SOL placeholder
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(maker_sol_account, false), // Maker's SOL account
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: create_offer_accounts,
        data: borsh_create_offer_in_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[create_offer_ix], Some(&context.1.pubkey()));
    transaction.sign(&[&context.1, &maker], context.2);
    context.0.process_transaction(transaction).await.unwrap();

    // Now accept the offer (Taker pays 5 SOL for 10 tokens)
    let accept_offer_ix_data = borsh::to_vec(&SwapInstruction::AcceptOffer).unwrap();

    let accept_offer_accounts = vec![
        AccountMeta::new(taker.pubkey(), true), // taker_account (signer)
        AccountMeta::new(offer_account_pubkey, true), // offer_account (writable)
        AccountMeta::new_readonly(maker.pubkey(), false), // maker_account (read-only)
        AccountMeta::new(maker_offer_token_account, false), // maker_token_account (writable)
        AccountMeta::new(taker_receive_token_account, false), // taker_token_account (writable)
        AccountMeta::new_readonly(offer_token_mint, false), // offer_token_mint
        AccountMeta::new_readonly(receive_token_mint, false), // receive_token_mint (SOL placeholder)
        AccountMeta::new_readonly(system_program::id(), false), // system_program
        AccountMeta::new_readonly(spl_token::id(), false),    // token_program
        AccountMeta::new(maker_sol_account, false), // maker_sol_account (writable, to receive SOL)
        AccountMeta::new(taker_sol_account, false), // taker_sol_account (writable, to pay SOL)
    ];

    let accept_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: accept_offer_accounts,
        data: accept_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[accept_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer, taker, and maker (these are the only signers in the instruction)
    transaction.sign(&[&context.1, &taker, &maker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // Verify balances after swap
    assert_eq!(
        get_token_balance(&mut context, &maker_offer_token_account).await,
        90
    ); // Maker's tokens decreased by 10
    assert_eq!(
        get_token_balance(&mut context, &taker_receive_token_account).await,
        10
    ); // Taker's tokens increased by 10
    assert_eq!(
        get_sol_balance(&mut context, &taker_sol_account).await,
        5_000_000_000
    ); // Taker's SOL decreased by 5
    assert_eq!(
        get_sol_balance(&mut context, &maker_sol_account).await,
        6_000_000_000
    ); // Maker's SOL increased by 5 (initial 1 SOL + 5 SOL from taker)

    // Verify offer status is Accepted
    let offer_account = context
        .0
        .get_account(offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();
    let offer_data = Offer::try_from_slice(&offer_account.data).unwrap();
    assert_eq!(offer_data.status, OfferStatus::Accepted);

    msg!("test_accept_public_sell_offer_success PASSED");
}

#[tokio::test]
async fn test_cancel_offer_success() {
    let mut program_test = ProgramTest::new(
        "soffer",
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        processor!(Processor::process),
    );

    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            ..Default::default()
        },
    );

    let mut context = program_test.start().await;

    let maker = Keypair::new();
    let mint_authority = Keypair::new();
    let offer_token_mint = Pubkey::new_from_array([0; 32]); // Maker offers SOL
    let receive_token_mint = create_mint(&mut context, &mint_authority, None, 0).await; // Maker wants this token

    // Maker's accounts
    let maker_sol_account = context.1.pubkey(); // Use payer's account for SOL

    // Create the offer (Maker offers 5 SOL for 10 tokens)
    let offer_sol_amount = 5_000_000_000; // 5 SOL
    let receive_token_amount = 10;
    let offer_type = OfferType::PublicBuy;

    let (offer_account_pubkey, bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            offer_token_mint.as_ref(), // SOL placeholder
            receive_token_mint.as_ref(),
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let create_offer_ix_data = borsh::to_vec(&SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount: offer_sol_amount,
        receive_token_amount,
        expiration: None,
        bump_seed,
    })
    .unwrap();

    let create_offer_accounts = vec![
        AccountMeta::new(maker.pubkey(), true),
        AccountMeta::new(offer_account_pubkey, false),
        AccountMeta::new_readonly(Pubkey::new_unique(), false), // Dummy token account, not used for SOL offer
        AccountMeta::new_readonly(offer_token_mint, false),     // SOL placeholder
        AccountMeta::new_readonly(receive_token_mint, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(maker_sol_account, false), // Maker's SOL account
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: create_offer_accounts,
        data: create_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[create_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer and maker (maker is the only signer in the instruction)
    transaction.sign(&[&context.1, &maker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // Verify SOL is in escrow
    let initial_offer_account_sol = get_sol_balance(&mut context, &offer_account_pubkey).await;
    assert_eq!(
        initial_offer_account_sol,
        offer_sol_amount
            + context
                .0
                .get_rent()
                .await
                .unwrap()
                .minimum_balance(Offer::MAX_LEN)
    ); // SOL + rent for PDA
    let initial_maker_sol_balance = get_sol_balance(&mut context, &maker_sol_account).await;
    assert_eq!(
        initial_maker_sol_balance,
        5_000_000_000
            - context
                .0
                .get_rent()
                .await
                .unwrap()
                .minimum_balance(Offer::MAX_LEN)
    ); // Maker's SOL decreased by escrow + rent

    // Now cancel the offer
    let cancel_offer_ix_data = borsh::to_vec(&SwapInstruction::CancelOffer).unwrap();

    let cancel_offer_accounts = vec![
        AccountMeta::new(maker.pubkey(), true), // offer_maker_account (signer)
        AccountMeta::new(offer_account_pubkey, true), // offer_account (writable)
        AccountMeta::new_readonly(system_program::id(), false), // system_program
        AccountMeta::new(maker_sol_account, false), // maker_sol_account (writable, to receive refund)
    ];

    let cancel_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: cancel_offer_accounts,
        data: cancel_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[cancel_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer and maker (maker is the only signer in the instruction)
    transaction.sign(&[&context.1, &maker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // Verify SOL is refunded and offer status is Declined
    let final_offer_account_sol = get_sol_balance(&mut context, &offer_account_pubkey).await;
    assert_eq!(
        final_offer_account_sol,
        context
            .0
            .get_rent()
            .await
            .unwrap()
            .minimum_balance(Offer::MAX_LEN)
    ); // Only rent remains
    let final_maker_sol_balance = get_sol_balance(&mut context, &maker_sol_account).await;
    assert_eq!(
        final_maker_sol_balance,
        10_000_000_000
            - context
                .0
                .get_rent()
                .await
                .unwrap()
                .minimum_balance(Offer::MAX_LEN)
    ); // Maker's SOL back to initial (minus rent for PDA)

    let offer_account = context
        .0
        .get_account(offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();
    let offer_data = Offer::try_from_slice(&offer_account.data).unwrap();
    assert_eq!(offer_data.status, OfferStatus::Declined);

    msg!("test_cancel_offer_success PASSED");
}

#[tokio::test]
async fn test_create_offer_insufficient_funds() {
    let mut program_test = ProgramTest::new(
        "soffer",
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        processor!(Processor::process),
    );
    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            ..Default::default()
        },
    );
    let mut context = program_test.start().await;

    let maker = Keypair::new();
    let mint_authority = Keypair::new();
    let offer_token_mint = create_mint(&mut context, &mint_authority, None, 0).await;
    let receive_token_mint = create_mint(&mut context, &mint_authority, None, 0).await;

    let maker_offer_token_account =
        create_token_account(&mut context, &maker, &offer_token_mint).await;
    // DO NOT mint tokens to maker_offer_token_account, simulating insufficient funds

    let maker_sol_account = context.1.pubkey();

    let offer_token_amount = 10; // Maker wants to offer 10 tokens
    let receive_token_amount = 5;
    let offer_type = OfferType::PublicSell; // Maker offers tokens

    let (offer_account_pubkey, bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            offer_token_mint.as_ref(),
            receive_token_mint.as_ref(),
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let instruction_data = SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount,
        receive_token_amount,
        expiration: None,
        bump_seed,
    };

    let accounts = vec![
        AccountMeta::new(maker.pubkey(), true),
        AccountMeta::new(offer_account_pubkey, false),
        AccountMeta::new(maker_offer_token_account, false),
        AccountMeta::new_readonly(offer_token_mint, false),
        AccountMeta::new_readonly(receive_token_mint, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(maker_sol_account, false),
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts,
        data: borsh::to_vec(&instruction_data).unwrap(),
    };

    let mut transaction =
        Transaction::new_with_payer(&[create_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer and maker (maker is the only signer in the instruction)
    transaction.sign(&[&context.1, &maker], context.2); // Only sign with actual signers

    // Expect an error: InsufficientFunds
    let err = context
        .0
        .process_transaction(transaction)
        .await
        .unwrap_err();
    assert_eq!(
        err.unwrap(),
        TransactionError::InstructionError(
            0, // instruction index, usually 0 for single-instruction tx
            InstructionError::Custom(SwapError::InsufficientFunds as u32)
        )
    );
    msg!("test_create_offer_insufficient_funds PASSED");
}

#[tokio::test]
async fn test_accept_offer_expired() {
    let mut program_test = ProgramTest::new(
        "soffer",
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        processor!(Processor::process),
    );

    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            ..Default::default()
        },
    );

    // Set a specific clock for testing expiration
    // program_test.set_sysvar_account::<solana_program::sysvar::clock::Clock>(
    //     &solana_program::sysvar::clock::Clock {
    //         unix_timestamp: 100, // Set initial time
    //         ..Default::default()
    //     }
    //     .into(),
    // );

    let mut context = program_test.start().await;

    let maker = Keypair::new();
    let taker = Keypair::new();
    let mint_authority = Keypair::new();
    let offer_token_mint = create_mint(&mut context, &mint_authority, None, 0).await;
    let receive_token_mint = Pubkey::new_from_array([0; 32]);

    let maker_offer_token_account =
        create_token_account(&mut context, &maker, &offer_token_mint).await;
    mint_to(
        &mut context,
        &offer_token_mint,
        &maker_offer_token_account,
        &mint_authority,
        100,
    )
    .await;
    let maker_sol_account = context.1.pubkey();
    // context.set_account(
    //     &maker_sol_account.pubkey(),
    //     &solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    // );

    let taker_receive_token_account =
        create_token_account(&mut context, &taker, &offer_token_mint).await;
    let taker_sol_account = context.1.pubkey();
    // context.set_account(
    //     &taker_sol_account.pubkey(),
    //     &solana_sdk::account::Account::new(10_000_000_000, 0, &system_program::id()),
    // );

    // Create the offer with an expiration in the past
    let offer_token_amount = 10;
    let receive_sol_amount = 5_000_000_000;
    let offer_type = OfferType::PublicSell;
    let expiration_time = 50; // Offer expires at time 50, but current time is 100

    let (offer_account_pubkey, bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            offer_token_mint.as_ref(),
            receive_token_mint.as_ref(),
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let create_offer_ix_data = borsh::to_vec(&SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount,
        receive_token_amount: receive_sol_amount,
        expiration: Some(expiration_time),
        bump_seed,
    })
    .unwrap();

    let create_offer_accounts = vec![
        AccountMeta::new(maker.pubkey(), true),
        AccountMeta::new(offer_account_pubkey, false),
        AccountMeta::new(maker_offer_token_account, false),
        AccountMeta::new_readonly(offer_token_mint, false),
        AccountMeta::new_readonly(receive_token_mint, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(maker_sol_account, false),
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: create_offer_accounts,
        data: create_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[create_offer_ix], Some(&context.1.pubkey()));
    transaction.sign(&[&context.1, &maker], context.2);
    context.0.process_transaction(transaction).await.unwrap();

    // Try to accept the expired offer
    let accept_offer_ix_data = borsh::to_vec(&SwapInstruction::AcceptOffer).unwrap();

    let accept_offer_accounts = vec![
        AccountMeta::new(taker.pubkey(), true),
        AccountMeta::new(offer_account_pubkey, true),
        AccountMeta::new_readonly(maker.pubkey(), false),
        AccountMeta::new(maker_offer_token_account, false),
        AccountMeta::new(taker_receive_token_account, false),
        AccountMeta::new_readonly(offer_token_mint, false),
        AccountMeta::new_readonly(receive_token_mint, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new(maker_sol_account, false),
        AccountMeta::new(taker_sol_account, false),
    ];

    let accept_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: accept_offer_accounts,
        data: accept_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[accept_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer, taker, and maker (these are the only signers in the instruction)
    transaction.sign(&[&context.1, &taker, &maker], context.2); // Only sign with actual signers

    // Expect an error: OfferExpired
    let err = context
        .0
        .process_transaction(transaction)
        .await
        .unwrap_err();
    assert_eq!(
        err.unwrap(),
        TransactionError::InstructionError(
            0, // instruction index, usually 0 for single-instruction tx
            InstructionError::Custom(SwapError::OfferExpired as u32)
        )
    );

    // Verify offer status is updated to Expired
    let offer_account = context
        .0
        .get_account(offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();
    let offer_data = Offer::try_from_slice(&offer_account.data).unwrap();
    assert_eq!(offer_data.status, OfferStatus::Expired);

    msg!("test_accept_offer_expired PASSED");
}

#[tokio::test]
async fn test_counter_offer_success() {
    let mut program_test = ProgramTest::new(
        "soffer",
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        processor!(Processor::process),
    );

    program_test.add_account(
        Pubkey::new_unique(),
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            ..Default::default()
        },
    );

    let mut context = program_test.start().await;

    let maker = Keypair::new();
    let taker = Keypair::new();
    let mint_authority = Keypair::new();
    let maker_token_mint = create_mint(&mut context, &mint_authority, None, 0).await;
    let taker_token_mint = create_mint(&mut context, &mint_authority, None, 0).await;

    // Maker's accounts (Maker offers maker_token_mint for taker_token_mint)
    let maker_offer_token_account =
        create_token_account(&mut context, &maker, &maker_token_mint).await;
    mint_to(
        &mut context,
        &maker_token_mint,
        &maker_offer_token_account,
        &mint_authority,
        100,
    )
    .await;
    let maker_sol_account = context.1.pubkey();
    // context.set_account(
    //     &maker_sol_account.pubkey(),
    //     &solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    // );

    // Taker's accounts (Taker would offer taker_token_mint)
    let taker_offer_token_account =
        create_token_account(&mut context, &taker, &taker_token_mint).await;
    mint_to(
        &mut context,
        &taker_token_mint,
        &taker_offer_token_account,
        &mint_authority,
        100,
    )
    .await;
    let taker_sol_account = context.1.pubkey();
    // context.set_account(
    //     &taker_sol_account.pubkey(),
    //     &solana_sdk::account::Account::new(1_000_000_000, 0, &system_program::id()),
    // );

    // Create initial offer (Maker offers 10 maker_token_mint for 5 taker_token_mint)
    let initial_offer_token_amount = 10;
    let initial_receive_token_amount = 5;
    let offer_type = OfferType::PublicSell;

    let (original_offer_account_pubkey, original_bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            maker.pubkey().as_ref(),
            maker_token_mint.as_ref(),
            taker_token_mint.as_ref(),
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let create_offer_ix_data = borsh::to_vec(&SwapInstruction::CreateOffer {
        offer_type,
        offer_token_amount: initial_offer_token_amount,
        receive_token_amount: initial_receive_token_amount,
        expiration: None,
        bump_seed: original_bump_seed,
    })
    .unwrap();

    let create_offer_accounts = vec![
        AccountMeta::new(maker.pubkey(), true),
        AccountMeta::new(original_offer_account_pubkey, false),
        AccountMeta::new(maker_offer_token_account, false),
        AccountMeta::new_readonly(maker_token_mint, false),
        AccountMeta::new_readonly(taker_token_mint, false),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(maker_sol_account, false),
    ];

    let create_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: create_offer_accounts,
        data: create_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[create_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer and maker (maker is the only signer in the instruction)
    transaction.sign(&[&context.1, &maker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // Taker makes a counter-offer: offers 7 taker_token_mint for 10 maker_token_mint
    let counter_offer_token_amount = 7;
    let counter_receive_token_amount = 10;

    let (new_offer_account_pubkey, new_bump_seed) = Pubkey::find_program_address(
        &[
            b"offer",
            taker.pubkey().as_ref(),   // Counter-maker is now taker
            taker_token_mint.as_ref(), // Taker offers taker_token_mint
            maker_token_mint.as_ref(), // Taker wants maker_token_mint
        ],
        &Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
    );

    let counter_offer_ix_data = borsh::to_vec(&SwapInstruction::CounterOffer {
        offer_token_amount: counter_offer_token_amount,
        receive_token_amount: counter_receive_token_amount,
        expiration: None,
        bump_seed: new_bump_seed,
    })
    .unwrap();

    let counter_offer_accounts = vec![
        AccountMeta::new(taker.pubkey(), true), // counter_maker_account (signer)
        AccountMeta::new(original_offer_account_pubkey, true), // original_offer_account (writable)
        AccountMeta::new(new_offer_account_pubkey, false), // new_offer_account (writable, PDA)
        AccountMeta::new(taker_offer_token_account, false), // counter_maker_token_account (writable)
        AccountMeta::new_readonly(taker_token_mint, false), // offer_token_mint (for counter)
        AccountMeta::new_readonly(maker_token_mint, false), // receive_token_mint (for counter)
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
        AccountMeta::new_readonly(solana_program::sysvar::rent::id(), false),
        AccountMeta::new(taker_sol_account, false), // counter_maker_sol_account (if offering SOL in counter)
        AccountMeta::new(maker_sol_account, false), // original_maker_sol_account (for refund if original had SOL escrow)
    ];

    let counter_offer_ix = Instruction {
        program_id: Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap(),
        accounts: counter_offer_accounts,
        data: counter_offer_ix_data,
    };

    let mut transaction =
        Transaction::new_with_payer(&[counter_offer_ix], Some(&context.1.pubkey()));
    // Only sign with payer and taker (taker is the only signer in the instruction)
    transaction.sign(&[&context.1, &taker], context.2); // Only sign with actual signers
    context.0.process_transaction(transaction).await.unwrap();

    // Verify original offer status is Countered
    let original_offer_account = context
        .0
        .get_account(original_offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();
    let original_offer_data = Offer::try_from_slice(&original_offer_account.data).unwrap();
    assert_eq!(original_offer_data.status, OfferStatus::Countered);

    // Verify new counter-offer account was created and contains correct data
    let new_offer_account = context
        .0
        .get_account(new_offer_account_pubkey)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        new_offer_account.owner,
        Pubkey::from_str("HpddKoiN2TNaJ8ZdWRVNbgLuAKop4JzYuEGPAM45agk8").unwrap()
    );

    let new_offer_data = Offer::try_from_slice(&new_offer_account.data).unwrap();
    assert_eq!(new_offer_data.offer_type, OfferType::PublicSell); // Type remains same as original
    assert_eq!(new_offer_data.status, OfferStatus::Active);
    assert_eq!(new_offer_data.maker, taker.pubkey()); // Taker is now the maker of the counter-offer
    assert_eq!(new_offer_data.offer_token_mint, taker_token_mint); // Taker offers taker_token_mint
    assert_eq!(
        new_offer_data.offer_token_amount,
        counter_offer_token_amount
    );
    assert_eq!(new_offer_data.receive_token_mint, maker_token_mint); // Taker wants maker_token_mint
    assert_eq!(
        new_offer_data.receive_token_amount,
        counter_receive_token_amount
    );
    assert_eq!(new_offer_data.is_counter_offer, true);
    assert_eq!(
        new_offer_data.original_offer_id,
        Some(original_offer_account_pubkey)
    );

    msg!("test_counter_offer_success PASSED");
}
