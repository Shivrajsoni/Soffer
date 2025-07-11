// This is like importing all the special tools we need for our smart contract.
// `solana_program` gives us the basic building blocks for Solana programs.
// `spl_token` gives us tools specifically for handling those shiny SPL tokens.
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{AccountInfo, next_account_info},
    entrypoint,
    entrypoint::ProgramResult,
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
    sysvar::{Sysvar, rent::Rent}, // To make sure accounts pay their "rent" on the blockchain
};

use spl_token::{
    id as spl_token_program_id,
    instruction::transfer_checked,          // To transfer tokens
    state::{Account as TokenAccount, Mint}, // To understand token accounts and token types
};
use std::convert::TryInto; // For converting numbers safely // Our new super-smart librarian for data!

// --- Error Handling ---
// This is like our list of "oops!" messages if something goes wrong.
// Each number is a unique "oops!" code.
#[derive(Debug, PartialEq)]
pub enum SwapError {
    InvalidInstruction,    // "Oops! I don't understand that button you pressed!"
    NotRentExempt,         // "Oops! This account doesn't have enough rent paid!"
    InvalidAccountData,    // "Oops! The data in this locker looks weird!"
    IncorrectOwner,        // "Oops! You're trying to use someone else's locker!"
    InsufficientFunds,     // "Oops! Not enough tokens/SOL for this trade!"
    InvalidOfferStatus,    // "Oops! This offer isn't active anymore!"
    OfferExpired,          // "Oops! This offer is too old!"
    Unauthorized,          // "Oops! You're not allowed to do that!"
    OfferMismatch,         // "Oops! The offer details don't match!"
    TokenMismatch,         // "Oops! You're trying to trade the wrong type of token!"
    AccountNotInitialized, // "Oops! This locker hasn't been set up yet!"
    InvalidProgramAddress, // "Oops! This account's special address isn't right!"
    MissingRequiredAccount,
    InvalidAccountInput, // "Oops! One of the accounts you gave me is not what I expected (e.g., wrong type or not writable)!"
    InvalidSystemProgram, // "Oops! The System Program address is wrong!"
    InvalidTokenProgram, // "Oops! The SPL Token Program address is wrong!" // "Oops! You forgot to give me an important locker!"
}

// We need to tell Solana how to turn our `SwapError` into a `ProgramError`.
impl From<SwapError> for ProgramError {
    fn from(e: SwapError) -> Self {
        ProgramError::Custom(e as u32) // We just convert our error into a special number.
    }
}

// --- State Management ---
// This is the blueprint for our "offer locker."
// It tells us what information each offer will hold.
// We add `#[derive(BorshSerialize, BorshDeserialize)]` so `borsh` can handle packing/unpacking!
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq)]
pub struct Offer {
    pub offer_type: OfferType, // Is it a direct offer, public buy, or public sell?
    pub status: OfferStatus,   // Is it active, accepted, countered, etc.?
    pub maker: Pubkey,         // The person who created the offer
    pub taker: Option<Pubkey>, // The person the direct offer is for (if any)
    pub offer_token_mint: Pubkey, // The type of token being offered (e.g., "ShinyCoin")
    pub offer_token_amount: u64, // How many tokens are being offered
    pub receive_token_mint: Pubkey, // The type of token expected in return (e.g., "GoldCoin" or SOL)
    pub receive_token_amount: u64,  // How many tokens/SOL are expected in return
    pub escrow_sol_amount: u64,     // How much SOL is held in escrow by the program for this offer
    pub expiration: Option<i64>,    // When the offer expires (optional)
    pub is_counter_offer: bool,     // Is this a counter-offer?
    pub original_offer_id: Option<Pubkey>, // If it's a counter, what was the original offer?
    pub bump_seed: u8,              // This is a special number for our PDA
}

impl Offer {
    // We'll calculate a reasonable max size for the offer account.
    // Borsh adds 1 byte for each Option<T> field.
    pub const MAX_LEN: usize = 1 // offer_type
        + 1 // status
        + 32 // maker
        + 1 + 32 // taker (Option<Pubkey>)
        + 32 // offer_token_mint
        + 8 // offer_token_amount
        + 32 // receive_token_mint
        + 8 // receive_token_amount
        + 8 // escrow_sol_amount
        + 1 + 8 // expiration (Option<i64>)
        + 1 // is_counter_offer
        + 1 + 32 // original_offer_id (Option<Pubkey>)
        + 1; // bump_seed
}

// Types of offers
#[repr(u8)] // This tells Rust to store these as simple numbers (0, 1, 2)
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq, Clone, Copy)] // Add Borsh and Clone/Copy
#[borsh(use_discriminant = true)]
pub enum OfferType {
    Direct = 0,     // An offer sent to a specific person
    PublicBuy = 1,  // "I want to buy X tokens for Y SOL" - anyone can accept
    PublicSell = 2, // "I want to sell X tokens for Y SOL" - anyone can accept
}

// Status of an offer
#[repr(u8)]
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq, Clone, Copy)] // Add Borsh and Clone/Copy
#[borsh(use_discriminant = true)]
pub enum OfferStatus {
    Active = 0,    // The offer is waiting to be accepted
    Accepted = 1,  // The offer has been completed
    Declined = 2,  // The offer was rejected
    Countered = 3, // A counter-offer was made
    Expired = 4,   // The offer timed out
}

// --- Instructions ---
// These are the "buttons" you can press on our vending machine.
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq)]
pub enum SwapInstruction {
    /// Create a new swap offer.
    /// Accounts:
    /// 0. `[signer]` maker_account: The person creating the offer.
    /// 1. `[writable]` offer_account: PDA for the offer data. Created by the program.
    /// 2. `[writable]` maker_token_account: Maker's token account for the token they are offering.
    /// 3. `[]` offer_token_mint: The mint account of the token being offered.
    /// 4. `[]` receive_token_mint: The mint account of the token/SOL expected in return.
    /// 5. `[]` system_program: Solana's System Program.
    /// 6. `[]` token_program: SPL Token Program.
    /// 7. `[]` rent_sysvar: Rent Sysvar.
    /// 8. `[writable]` (optional) maker_sol_account: Maker's SOL account (if offering SOL or receiving SOL).
    /// 9. `[]` (optional) taker_account: The specific person for a direct offer.
    CreateOffer {
        offer_type: OfferType,
        offer_token_amount: u64,
        receive_token_amount: u64,
        expiration: Option<i64>,
        bump_seed: u8, // The bump seed for the offer_account PDA
    },
    /// Accept an existing swap offer.
    /// Accounts:
    /// 0. `[signer]` taker_account: The person accepting the offer.
    /// 1. `[writable]` offer_account: The PDA for the offer data.
    /// 2. `[writable]` maker_account: The original offer maker's account.
    /// 3. `[writable]` maker_token_account: Maker's token account for the token they are giving/receiving.
    /// 4. `[writable]` taker_token_account: Taker's token account for the token they are giving/receiving.
    /// 5. `[]` offer_token_mint: The mint account of the token offered by the maker.
    /// 6. `[]` receive_token_mint: The mint account of the token expected by the maker (given by taker).
    /// 7. `[]` system_program: Solana's System Program.
    /// 8. `[]` token_program: SPL Token Program.
    /// 9. `[writable]` (optional) maker_sol_account: Maker's SOL account (if involved in SOL transfer).
    /// 10. `[writable]` (optional) taker_sol_account: Taker's SOL account (if involved in SOL transfer).
    AcceptOffer,
    /// Create a counter-offer to an existing offer.
    /// Accounts:
    /// 0. `[signer]` counter_maker_account: The person making the counter-offer.
    /// 1. `[writable]` original_offer_account: The PDA for the original offer data.
    /// 2. `[writable]` new_offer_account: PDA for the new counter-offer data. Created by the program.
    /// 3. `[writable]` counter_maker_token_account: Counter-maker's token account for the token they are offering.
    /// 4. `[]` offer_token_mint: The mint account of the token being offered in the counter.
    /// 5. `[]` receive_token_mint: The mint account of the token/SOL expected in return in the counter.
    /// 6. `[]` system_program: Solana's System Program.
    /// 7. `[]` token_program: SPL Token Program.
    /// 8. `[]` rent_sysvar: Rent Sysvar.
    /// 9. `[writable]` (optional) counter_maker_sol_account: Counter-maker's SOL account (if offering SOL or receiving SOL).
    /// 10. `[writable]` (optional) original_maker_sol_account: Original maker's SOL account (for refund of escrowed SOL).
    CounterOffer {
        offer_token_amount: u64,
        receive_token_amount: u64,
        expiration: Option<i64>,
        bump_seed: u8, // The bump seed for the new_offer_account PDA
    },
    /// Cancel an existing offer.
    /// Accounts:
    /// 0. `[signer]` offer_maker_account: The person who made the offer.
    /// 1. `[writable]` offer_account: The PDA for the offer data.
    /// 2. `[]` system_program: Solana's System Program.
    /// 3. `[writable]` (optional) maker_sol_account: Maker's SOL account (to refund escrowed SOL).
    CancelOffer,
}

// --- Processor (The Brain of Our Vending Machine) ---
// This is where all the magic happens! It takes an instruction and figures out what to do.
pub struct Processor;

impl Processor {
    // This is the main function that Solana calls when someone interacts with our program.
    pub fn process(
        program_id: &Pubkey,      // Our program's address
        accounts: &[AccountInfo], // All the lockers involved in this transaction
        instruction_data: &[u8],  // The "button" pressed and any extra info
    ) -> ProgramResult {
        // First, let's figure out which "button" was pressed.
        let instruction = SwapInstruction::try_from_slice(instruction_data) // Use borsh to unpack!
            .map_err(|_| SwapError::InvalidInstruction)?;

        // Now, based on the button, we call the right function.
        match instruction {
            SwapInstruction::CreateOffer {
                offer_type,
                offer_token_amount,
                receive_token_amount,
                expiration,
                bump_seed,
            } => {
                msg!("Instruction: CreateOffer");
                Self::process_create_offer(
                    program_id,
                    accounts,
                    offer_type,
                    offer_token_amount,
                    receive_token_amount,
                    expiration,
                    bump_seed,
                )
            }
            SwapInstruction::AcceptOffer => {
                msg!("Instruction: AcceptOffer");
                Self::process_accept_offer(program_id, accounts)
            }
            SwapInstruction::CounterOffer {
                offer_token_amount,
                receive_token_amount,
                expiration,
                bump_seed,
            } => {
                msg!("Instruction: CounterOffer");
                Self::process_counter_offer(
                    program_id,
                    accounts,
                    offer_token_amount,
                    receive_token_amount,
                    expiration,
                    bump_seed,
                )
            }
            SwapInstruction::CancelOffer => {
                msg!("Instruction: CancelOffer");
                Self::process_cancel_offer(program_id, accounts)
            }
        }
    }

    // --- Helper function to transfer SOL (money) ---
    fn transfer_sol(
        account_infos: &[AccountInfo], // [from_account, to_account, system_program]
        amount: u64,
        signer_seeds: Option<&[&[u8]]>,
    ) -> ProgramResult {
        let from_account = &account_infos[0];
        let to_account = &account_infos[1];
        let system_program = &account_infos[2];
        // Basic checks for SOL transfer accounts
        if !from_account.is_writable {
            return Err(SwapError::InvalidAccountInput.into());
        }
        if !to_account.is_writable {
            return Err(SwapError::InvalidAccountInput.into());
        }
        if system_program.key != &solana_program::system_program::ID {
            return Err(SwapError::InvalidSystemProgram.into());
        }
        if from_account.lamports() < amount {
            return Err(SwapError::InsufficientFunds.into());
        }
        // Create an instruction to transfer SOL.
        let transfer_instruction = system_instruction::transfer(
            from_account.key, // From whom
            to_account.key,   // To whom
            amount,           // How much
        );
        // Call Solana's system program to actually do the transfer.
        if let Some(seeds) = signer_seeds {
            invoke_signed(&transfer_instruction, account_infos, &[seeds])?;
        } else {
            invoke(&transfer_instruction, account_infos)?;
        }
        Ok(())
    }

    // --- Helper function to transfer SPL Tokens (shiny cards) ---
    // This function helps us move tokens between accounts.
    fn transfer_spl_token(
        account_infos: &[AccountInfo], // [from_token_account, mint_account, to_token_account, from_authority, token_program]
        amount: u64,
        mint_decimals: u8,
        signer_seeds: Option<&[&[u8]]>,
    ) -> ProgramResult {
        let from_token_account = &account_infos[0];
        let mint_account = &account_infos[1];
        let to_token_account = &account_infos[2];
        let from_authority = &account_infos[3];
        let token_program = &account_infos[4];
        if !from_token_account.is_writable {
            return Err(SwapError::InvalidAccountInput.into());
        }
        if !to_token_account.is_writable {
            return Err(SwapError::InvalidAccountInput.into());
        }
        if token_program.key != &spl_token_program_id() {
            return Err(SwapError::InvalidTokenProgram.into());
        }
        // Authority should be a signer if not program-signed
        if signer_seeds.is_none() && !from_authority.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        // Create an instruction to transfer tokens.
        let transfer_instruction = transfer_checked(
            token_program.key,      // The SPL Token program's address
            from_token_account.key, // From this token account
            mint_account.key,       // The token mint (type of token)
            to_token_account.key,   // To this token account
            from_authority.key,
            &[],
            amount,
            mint_decimals,
        )?;
        // Call the SPL Token program to actually do the transfer.
        if let Some(seeds) = signer_seeds {
            invoke_signed(&transfer_instruction, account_infos, &[seeds])?;
        } else {
            invoke(&transfer_instruction, account_infos)?;
        }
        Ok(())
    }

    // --- Process CreateOffer Instruction ---
    fn process_create_offer(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        offer_type: OfferType,
        offer_token_amount: u64,
        receive_token_amount: u64,
        expiration: Option<i64>,
        bump_seed: u8,
    ) -> ProgramResult {
        msg!("Processing CreateOffer...");
        let account_info_iter = &mut accounts.iter();

        // Get all the lockers we need from the list.
        let maker_account = next_account_info(account_info_iter)?; // The person making the offer
        let offer_account = next_account_info(account_info_iter)?; // The new locker for this offer (PDA)
        let maker_token_account = next_account_info(account_info_iter)?; // Maker's token account (for what they offer)
        let offer_token_mint = next_account_info(account_info_iter)?; // The type of token being offered
        let receive_token_mint = next_account_info(account_info_iter)?; // The type of token/SOL expected
        let system_program = next_account_info(account_info_iter)?; // Solana's basic program
        let token_program = next_account_info(account_info_iter)?; // SPL Token program
        let rent_sysvar = next_account_info(account_info_iter)?; // Rent checker

        // Optional accounts
        let maker_sol_account_opt = next_account_info(account_info_iter).ok();
        let taker_account_opt = next_account_info(account_info_iter).ok();

        // --- Basic Checks ---
        // 1. Is the maker signing this?
        if !maker_account.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }

        // 2. Verify the offer_account is a PDA derived from our program.
        let offer_seeds = &[
            b"offer",                        // A constant string seed
            maker_account.key.as_ref(),      // Maker's public key as a seed
            offer_token_mint.key.as_ref(),   // Offered token mint as a seed
            receive_token_mint.key.as_ref(), // Received token mint as a seed
            &[bump_seed],                    // The bump seed
        ];
        let (expected_offer_key, expected_bump_seed) =
            Pubkey::find_program_address(offer_seeds, program_id);

        if expected_offer_key != *offer_account.key || expected_bump_seed != bump_seed {
            return Err(SwapError::InvalidProgramAddress.into());
        }

        // 3. Create the offer account if it doesn't exist and is not rent-exempt.
        // The offer_account must be writable and owned by the system program for creation.
        if offer_account.data_len() == 0 {
            let space = Offer::MAX_LEN; // Max size for our offer data
            let rent = &Rent::from_account_info(rent_sysvar)?;
            let rent_lamports = rent.minimum_balance(space);

            invoke_signed(
                &system_instruction::create_account(
                    maker_account.key, // Payer
                    offer_account.key, // New account address (PDA)
                    rent_lamports,     // Rent
                    space as u64,      // Size
                    program_id,        // Owner
                ),
                &[
                    maker_account.clone(),
                    offer_account.clone(),
                    system_program.clone(),
                ],
                &[offer_seeds], // Sign with the PDA seeds
            )?;
        } else {
            // If account already exists, it must be owned by our program and empty
            if offer_account.owner != program_id || !offer_account.data_is_empty() {
                return Err(SwapError::InvalidAccountData.into());
            }
        }

        // 4. Check if maker_token_account is actually a token account and owned by maker.
        let maker_token_account_data = TokenAccount::unpack(&maker_token_account.data.borrow())?;
        if maker_token_account_data.owner != *maker_account.key {
            return Err(SwapError::IncorrectOwner.into());
        }
        if maker_token_account_data.mint != *offer_token_mint.key {
            return Err(SwapError::TokenMismatch.into());
        }

        // --- Handle Direct Offers ---
        let taker_pubkey = if offer_type == OfferType::Direct {
            let taker_account = taker_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            Some(*taker_account.key)
        } else {
            None
        };

        // --- Escrow SOL if it's a "Buy" offer (maker offers SOL for tokens) ---
        let mut escrow_sol = 0;
        // Using a dummy Pubkey::new_from_array([0; 32]) to represent SOL.
        // In a real app, consider using spl_token::native_mint::ID for wrapped SOL or a specific flag.
        if offer_type == OfferType::PublicBuy
            || (offer_type == OfferType::Direct
                && *offer_token_mint.key == Pubkey::new_from_array([0; 32]))
        {
            // If the maker is offering SOL, they need to send it to our program's escrow.
            escrow_sol = offer_token_amount; // The amount of SOL they are offering

            let maker_sol_account =
                maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *maker_sol_account.key != *maker_account.key {
                return Err(SwapError::IncorrectOwner.into()); // Ensure it's the maker's SOL account
            }

            msg!("Transferring {} SOL to escrow...", escrow_sol);
            Self::transfer_sol(
                &[
                    maker_sol_account.clone(),
                    offer_account.clone(),
                    system_program.clone(),
                ],
                escrow_sol,
                None, // Not signed by program
            )?;
            msg!("SOL transferred to escrow.");
        } else if offer_token_amount > maker_token_account_data.amount {
            // If maker is offering tokens, they must have enough.
            return Err(SwapError::InsufficientFunds.into());
        }

        // --- Create and Save the Offer Data ---
        let offer = Offer {
            offer_type,
            status: OfferStatus::Active, // New offers are always active
            maker: *maker_account.key,
            taker: taker_pubkey,
            offer_token_mint: *offer_token_mint.key,
            offer_token_amount,
            receive_token_mint: *receive_token_mint.key,
            receive_token_amount,
            escrow_sol_amount: escrow_sol,
            expiration,
            is_counter_offer: false,
            original_offer_id: None,
            bump_seed, // Store the bump seed in the offer data
        };

        // Save the offer data into the `offer_account` locker using borsh.
        offer.serialize(&mut &mut offer_account.data.borrow_mut()[..])?;

        msg!("Offer created successfully!");
        Ok(())
    }

    // --- Process AcceptOffer Instruction ---
    fn process_accept_offer(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
        msg!("Processing AcceptOffer...");
        let account_info_iter = &mut accounts.iter();

        // Get all the lockers we need.
        let taker_account = next_account_info(account_info_iter)?; // The person accepting
        let offer_account = next_account_info(account_info_iter)?; // The offer's locker (PDA)
        let maker_account = next_account_info(account_info_iter)?; // The original maker
        let maker_token_account = next_account_info(account_info_iter)?; // Maker's token account
        let taker_token_account = next_account_info(account_info_iter)?; // Taker's token account
        let offer_token_mint = next_account_info(account_info_iter)?; // Offered token type (mint)
        let receive_token_mint = next_account_info(account_info_iter)?; // Received token type (mint)
        let system_program = next_account_info(account_info_iter)?; // System program
        let token_program = next_account_info(account_info_iter)?; // Token program

        // Optional accounts for SOL transfers
        let maker_sol_account_opt = next_account_info(account_info_iter).ok();
        let taker_sol_account_opt = next_account_info(account_info_iter).ok();

        // --- Basic Checks ---
        if !taker_account.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        if offer_account.owner != program_id {
            return Err(SwapError::IncorrectOwner.into());
        }

        // Load the offer data from its locker using borsh.
        let mut offer_data = Offer::try_from_slice(&offer_account.data.borrow())?;

        // Verify the offer_account is a PDA derived from our program and the stored bump seed.
        let offer_seeds = &[
            b"offer",
            offer_data.maker.as_ref(),
            offer_data.offer_token_mint.as_ref(),
            offer_data.receive_token_mint.as_ref(),
            &[offer_data.bump_seed],
        ];
        let (expected_offer_key, _) = Pubkey::find_program_address(offer_seeds, program_id);

        if expected_offer_key != *offer_account.key {
            return Err(SwapError::InvalidProgramAddress.into());
        }

        // Check offer status and expiration.
        if offer_data.status != OfferStatus::Active {
            return Err(SwapError::InvalidOfferStatus.into());
        }
        if let Some(exp) = offer_data.expiration {
            if solana_program::clock::Clock::get()?.unix_timestamp > exp {
                offer_data.status = OfferStatus::Expired;
                offer_data.serialize(&mut &mut offer_account.data.borrow_mut()[..])?;
                return Err(SwapError::OfferExpired.into());
            }
        }

        // Check if it's a direct offer and the taker is correct.
        if offer_data.offer_type == OfferType::Direct
            && offer_data.taker != Some(*taker_account.key)
        {
            return Err(SwapError::Unauthorized.into());
        }

        // Verify maker_account is the actual maker.
        if offer_data.maker != *maker_account.key {
            return Err(SwapError::OfferMismatch.into());
        }

        // Check token account ownership and mints
        let maker_token_account_data = TokenAccount::unpack(&maker_token_account.data.borrow())?;
        let taker_token_account_data = TokenAccount::unpack(&taker_token_account.data.borrow())?;

        if maker_token_account_data.owner != *maker_account.key {
            return Err(SwapError::IncorrectOwner.into());
        }
        if taker_token_account_data.owner != *taker_account.key {
            return Err(SwapError::IncorrectOwner.into());
        }

        // --- Perform the Swap! ---
        // Case 1: Maker offered SOL (escrow_sol_amount > 0), Taker offers Tokens
        if offer_data.escrow_sol_amount > 0 {
            msg!("Executing SOL for Token swap...");

            // Ensure correct mints for token accounts
            if maker_token_account_data.mint != *receive_token_mint.key {
                return Err(SwapError::TokenMismatch.into());
            }
            if taker_token_account_data.mint != *offer_token_mint.key {
                // Taker gives tokens (offer_token_mint)
                return Err(SwapError::TokenMismatch.into());
            }

            // Transfer SOL from escrow (offer_account) to maker_account
            let maker_sol_account =
                maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *maker_sol_account.key != *maker_account.key {
                return Err(SwapError::IncorrectOwner.into());
            }
            Self::transfer_sol(
                &[
                    offer_account.clone(),
                    maker_sol_account.clone(),
                    system_program.clone(),
                ],
                offer_data.escrow_sol_amount,
                Some(offer_seeds), // Program is signing for the escrow account
            )?;

            // Transfer tokens from taker to maker
            let mint_info = Mint::unpack(&receive_token_mint.data.borrow())?; // Get decimals for the token taker is giving
            Self::transfer_spl_token(
                &[
                    taker_token_account.clone(),
                    receive_token_mint.clone(),
                    maker_token_account.clone(),
                    taker_account.clone(),
                    token_program.clone(),
                ],
                offer_data.receive_token_amount,
                mint_info.decimals,
                None, // Taker is signing directly
            )?;
            msg!("SOL for Token swap completed.");
        } else {
            // Case 2: Maker offered Tokens (escrow_sol_amount == 0), Taker offers SOL
            msg!("Executing Token for SOL swap...");

            // Ensure correct mints for token accounts
            if maker_token_account_data.mint != *offer_token_mint.key {
                // Maker gives tokens (offer_token_mint)
                return Err(SwapError::TokenMismatch.into());
            }
            if taker_token_account_data.mint != *receive_token_mint.key {
                return Err(SwapError::TokenMismatch.into());
            }

            // Transfer tokens from maker to taker
            let mint_info = Mint::unpack(&offer_token_mint.data.borrow())?; // Get decimals for the token maker is giving
            Self::transfer_spl_token(
                &[
                    maker_token_account.clone(),
                    offer_token_mint.clone(),
                    taker_token_account.clone(),
                    maker_account.clone(),
                    token_program.clone(),
                ],
                offer_data.offer_token_amount,
                mint_info.decimals,
                None, // Maker is signing directly
            )?;

            // Transfer SOL from taker to maker
            let taker_sol_account =
                taker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            let maker_sol_account =
                maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *taker_sol_account.key != *taker_account.key
                || *maker_sol_account.key != *maker_account.key
            {
                return Err(SwapError::IncorrectOwner.into());
            }
            Self::transfer_sol(
                &[
                    taker_sol_account.clone(),
                    maker_sol_account.clone(),
                    system_program.clone(),
                ],
                offer_data.receive_token_amount,
                None, // Not signed by program
            )?;
            msg!("Token for SOL swap completed.");
        }

        // Update offer status to Accepted.
        offer_data.status = OfferStatus::Accepted;
        offer_data.serialize(&mut &mut offer_account.data.borrow_mut()[..])?; // Use borsh to pack!

        msg!("Offer accepted successfully!");
        Ok(())
    }

    // --- Process CounterOffer Instruction ---
    fn process_counter_offer(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        offer_token_amount: u64,
        receive_token_amount: u64,
        expiration: Option<i64>,
        bump_seed: u8,
    ) -> ProgramResult {
        msg!("Processing CounterOffer...");
        let account_info_iter = &mut accounts.iter();

        let counter_maker_account = next_account_info(account_info_iter)?; // The person making the counter
        let original_offer_account = next_account_info(account_info_iter)?; // The original offer's locker (PDA)
        let new_offer_account = next_account_info(account_info_iter)?; // New locker for the counter-offer (PDA)
        let counter_maker_token_account = next_account_info(account_info_iter)?; // Counter-maker's token account
        let offer_token_mint = next_account_info(account_info_iter)?; // Token offered in counter
        let receive_token_mint = next_account_info(account_info_iter)?; // Token received in counter
        let system_program = next_account_info(account_info_iter)?;
        let token_program = next_account_info(account_info_iter)?;
        let rent_sysvar = next_account_info(account_info_iter)?;

        // Optional accounts
        let counter_maker_sol_account_opt = next_account_info(account_info_iter).ok();
        let original_maker_sol_account_opt = next_account_info(account_info_iter).ok();

        // --- Basic Checks ---
        if !counter_maker_account.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        if original_offer_account.owner != program_id {
            return Err(SwapError::IncorrectOwner.into());
        }

        let mut original_offer_data = Offer::try_from_slice(&original_offer_account.data.borrow())?;

        // Verify original_offer_account PDA
        let original_offer_seeds = &[
            b"offer",
            original_offer_data.maker.as_ref(),
            original_offer_data.offer_token_mint.as_ref(),
            original_offer_data.receive_token_mint.as_ref(),
            &[original_offer_data.bump_seed],
        ];
        let (expected_original_offer_key, _) =
            Pubkey::find_program_address(original_offer_seeds, program_id);
        if expected_original_offer_key != *original_offer_account.key {
            return Err(SwapError::InvalidProgramAddress.into());
        }

        // Check if the counter-maker is either the original maker or the original taker.
        if *counter_maker_account.key != original_offer_data.maker
            && original_offer_data.taker != Some(*counter_maker_account.key)
        {
            return Err(SwapError::Unauthorized.into());
        }

        // Check if the original offer is active.
        if original_offer_data.status != OfferStatus::Active {
            return Err(SwapError::InvalidOfferStatus.into());
        }

        // --- Handle Escrowed SOL from Original Offer ---
        if original_offer_data.escrow_sol_amount > 0 {
            // If the original offer had SOL in escrow, refund it to the original maker.
            let original_maker_sol_account =
                original_maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *original_maker_sol_account.key != original_offer_data.maker {
                return Err(SwapError::IncorrectOwner.into());
            }

            msg!(
                "Refunding {} SOL from original offer escrow to original maker.",
                original_offer_data.escrow_sol_amount
            );
            Self::transfer_sol(
                &[
                    original_offer_account.clone(),
                    original_maker_sol_account.clone(),
                    system_program.clone(),
                ],
                original_offer_data.escrow_sol_amount,
                Some(original_offer_seeds), // Program is signing for the escrow account
            )?;
            original_offer_data.escrow_sol_amount = 0; // Clear escrow amount
        }

        // --- Create New Counter-Offer Account (PDA) ---
        let new_offer_seeds = &[
            b"offer",
            counter_maker_account.key.as_ref(),
            offer_token_mint.key.as_ref(),
            receive_token_mint.key.as_ref(),
            &[bump_seed],
        ];
        let (expected_new_offer_key, expected_bump_seed) =
            Pubkey::find_program_address(new_offer_seeds, program_id);

        if expected_new_offer_key != *new_offer_account.key || expected_bump_seed != bump_seed {
            return Err(SwapError::InvalidProgramAddress.into());
        }

        if new_offer_account.data_len() == 0 {
            let space = Offer::MAX_LEN;
            let rent = &Rent::from_account_info(rent_sysvar)?;
            let rent_lamports = rent.minimum_balance(space);

            invoke_signed(
                &system_instruction::create_account(
                    counter_maker_account.key, // Payer
                    new_offer_account.key,     // New account address (PDA)
                    rent_lamports,             // Rent
                    space as u64,              // Size
                    program_id,                // Owner
                ),
                &[
                    counter_maker_account.clone(),
                    new_offer_account.clone(),
                    system_program.clone(),
                ],
                &[new_offer_seeds], // Sign with the PDA seeds
            )?;
        } else {
            if new_offer_account.owner != program_id || !new_offer_account.data_is_empty() {
                return Err(SwapError::InvalidAccountData.into());
            }
        }

        // --- Escrow SOL for the New Counter-Offer if applicable ---
        let mut new_escrow_sol = 0;
        if *offer_token_mint.key == Pubkey::new_from_array([0; 32]) {
            // If counter-maker offers SOL
            new_escrow_sol = offer_token_amount;
            let counter_maker_sol_account =
                counter_maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *counter_maker_sol_account.key != *counter_maker_account.key {
                return Err(SwapError::IncorrectOwner.into());
            }

            msg!(
                "Transferring {} SOL to new counter-offer escrow...",
                new_escrow_sol
            );
            Self::transfer_sol(
                &[
                    counter_maker_sol_account.clone(),
                    new_offer_account.clone(),
                    system_program.clone(),
                ],
                new_escrow_sol,
                None, // Not signed by program
            )?;
            msg!("SOL transferred to new escrow.");
        } else {
            // Check if counter-maker has enough tokens if they are offering tokens.
            let counter_maker_token_account_data =
                TokenAccount::unpack(&counter_maker_token_account.data.borrow())?;
            if offer_token_amount > counter_maker_token_account_data.amount {
                return Err(SwapError::InsufficientFunds.into());
            }
            if counter_maker_token_account_data.mint != *offer_token_mint.key {
                return Err(SwapError::TokenMismatch.into());
            }
        }

        // --- Create and Save the New Counter Offer Data ---
        let counter_offer = Offer {
            offer_type: original_offer_data.offer_type, // Keep the same type (direct/public)
            status: OfferStatus::Active,
            maker: *counter_maker_account.key,
            taker: if original_offer_data.maker == *counter_maker_account.key {
                original_offer_data.taker // If maker countered, taker is still the same
            } else {
                Some(original_offer_data.maker) // If taker countered, maker is now the taker
            },
            offer_token_mint: *offer_token_mint.key,
            offer_token_amount,
            receive_token_mint: *receive_token_mint.key,
            receive_token_amount,
            escrow_sol_amount: new_escrow_sol,
            expiration,
            is_counter_offer: true,
            original_offer_id: Some(*original_offer_account.key),
            bump_seed,
        };

        counter_offer.serialize(&mut &mut new_offer_account.data.borrow_mut()[..])?;

        // Update the original offer's status to Countered.
        original_offer_data.status = OfferStatus::Countered;
        original_offer_data.serialize(&mut &mut original_offer_account.data.borrow_mut()[..])?;

        msg!("Counter-offer created successfully!");
        Ok(())
    }

    // --- Process CancelOffer Instruction ---
    fn process_cancel_offer(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
        msg!("Processing CancelOffer...");
        let account_info_iter = &mut accounts.iter();

        let offer_maker_account = next_account_info(account_info_iter)?; // The person cancelling
        let offer_account = next_account_info(account_info_iter)?; // The offer's locker (PDA)
        let system_program = next_account_info(account_info_iter)?;

        // Optional account for SOL refund
        let maker_sol_account_opt = next_account_info(account_info_iter).ok();

        // --- Basic Checks ---
        if !offer_maker_account.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        if offer_account.owner != program_id {
            return Err(SwapError::IncorrectOwner.into());
        }

        let mut offer_data = Offer::try_from_slice(&offer_account.data.borrow())?;

        // Verify offer_account PDA
        let offer_seeds = &[
            b"offer",
            offer_data.maker.as_ref(),
            offer_data.offer_token_mint.as_ref(),
            offer_data.receive_token_mint.as_ref(),
            &[offer_data.bump_seed],
        ];
        let (expected_offer_key, _) = Pubkey::find_program_address(offer_seeds, program_id);
        if expected_offer_key != *offer_account.key {
            return Err(SwapError::InvalidProgramAddress.into());
        }

        // Only the original maker can cancel an offer.
        if offer_data.maker != *offer_maker_account.key {
            return Err(SwapError::Unauthorized.into());
        }

        // Only active offers can be cancelled.
        if offer_data.status != OfferStatus::Active {
            return Err(SwapError::InvalidOfferStatus.into());
        }

        // Refund any escrowed SOL.
        if offer_data.escrow_sol_amount > 0 {
            let maker_sol_account =
                maker_sol_account_opt.ok_or(SwapError::MissingRequiredAccount)?;
            if *maker_sol_account.key != *offer_maker_account.key {
                return Err(SwapError::IncorrectOwner.into());
            }

            msg!(
                "Refunding {} SOL from escrow to maker...",
                offer_data.escrow_sol_amount
            );
            Self::transfer_sol(
                &[
                    offer_account.clone(),
                    maker_sol_account.clone(),
                    system_program.clone(),
                ],
                offer_data.escrow_sol_amount,
                Some(offer_seeds), // Program is signing for the escrow account
            )?;
            offer_data.escrow_sol_amount = 0; // Clear the escrowed amount
        }

        // Set offer status to Declined.
        offer_data.status = OfferStatus::Declined;
        offer_data.serialize(&mut &mut offer_account.data.borrow_mut()[..])?;

        msg!("Offer cancelled successfully!");
        Ok(())
    }
}

entrypoint!(process_instruction);
pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    // Just call our `Processor`'s main function.
    Processor::process(program_id, accounts, instruction_data)
}
