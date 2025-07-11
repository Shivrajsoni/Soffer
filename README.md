# Develop a Solana smart contract (program) that enables token swaps between users with the following features:

- Escrow-based swapping
- Public and direct offers
- Counter-offer functionality
- Token holder discovery
- Listing and accepting offers

⸻

Core Features

1. Direct Offers to Token Holders

- Users can query token holders (via off-chain indexing or on-chain metadata).
- A user can send a direct offer to a specific token holder (e.g., offer to buy 1000 tokens for 1 SOL).
- The offered SOL is locked in escrow by the program.
- If the token holder accepts, the swap executes:
- Tokens are transferred from holder to buyer.
- Escrowed SOL is transferred from program to the token holder.

2. Public Listings (Buy or Sell)

- Users can create public offers:
- Sell offer: Offering X tokens in exchange for Y SOL.
- Buy offer: Offering Y SOL in exchange for X tokens.
- Offers are visible to all users.
- Any user can accept a public offer:
- On acceptance, the swap executes via the escrow mechanism.

3. Counter-Offers

- Any offer (direct or public) can receive a counter-offer from the other party.
- Counter-offers can modify token amount, SOL amount, or both.
- The original offerer can then:
- Accept the counter-offer (executes swap).
- Decline or propose another counter.

4. Offer Management

- Each user can:
- View all public offers (buy/sell).
- View direct offers they’ve made or received.
- View and respond to counter-offers.
- Offers have optional expiration timestamps and statuses (active, accepted, declined, countered, expired).

⸻

Program Responsibilities

- Maintain an escrow for all in-progress offers.
- Ensure atomic swaps between tokens and SOL.
- Validate token ownership and balances.
- Manage state for:
- Offers and counter-offers
- Escrowed funds
- Offer status and metadata
