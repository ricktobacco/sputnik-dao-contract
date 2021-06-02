use near_contract_standards::fungible_token::core_impl::ext_fungible_token;
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::LookupMap;
use near_sdk::json_types::{ValidAccountId, WrappedDuration, U128};
use near_sdk::{
    env, log, ext_contract, near_bindgen, AccountId, Balance, BorshStorageKey, Duration, Gas,
    PanicOnDefault, Promise, PromiseOrValue, PromiseResult,
};

pub use user::{User, VersionedUser};

mod storage_impl;
mod user;

#[derive(BorshStorageKey, BorshSerialize)]
enum StorageKeys {
    Users,
    Votes
}

/// Amount of gas for fungible token transfers.
pub const GAS_FOR_FT_TRANSFER: Gas = 10_000_000_000_000;

/// Amount of gas for delegate action.
pub const GAS_FOR_DELEGATE: Gas = 10_000_000_000_000;

/// Amount of gas for register action.
pub const GAS_FOR_REGISTER: Gas = 10_000_000_000_000;

/// Amount of gas for undelegate action.
pub const GAS_FOR_UNDELEGATE: Gas = 10_000_000_000_000;

/// Amount of gas for undelegate action.
pub const TMP_GAS: Gas = 10_000_000_000_000;

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct WeightsData {
	total: Balance,
	// approximate index of median 
	k: u32,
	// sum(W[0..k])
	sum_w_k: Balance, 
	// all the distinct votes for given property
	y: Vec<i128>,
	// all the weights associated with the votes
	w: Vec<Balance>,
}
impl WeightsData {
    pub fn new() -> Self {
        Self {
            total: 0, k: 0, sum_w_k: 0, y: Vec::new(), w: Vec::new()
        }
    }
}

#[ext_contract(ext_sputnik)]
pub trait Sputnik {
    fn register_delegation(&mut self, account_id: AccountId);
    fn delegate(&mut self, account_id: AccountId, amount: U128);
    fn undelegate(&mut self, account_id: AccountId, amount: U128);
    fn get_delegation_balances(&self, account_id: AccountId) -> (Balance, Balance);
}

#[near_bindgen]
#[derive(BorshSerialize, BorshDeserialize, PanicOnDefault)]
pub struct Contract {
    /// DAO owner of this staking contract.
    owner_id: AccountId,
    /// Vote token account.
    vote_token_id: AccountId,
    /// Recording user deposits.
    users: LookupMap<AccountId, VersionedUser>,
    /// Total token amount deposited.
    total_amount: Balance,
    /// Duration of unstaking. Should be over the possible voting periods.
    unstake_period: Duration,
    /// Result of rebalance function, AKA medianizer
    median: i128, 
    /// d stands for Data, used in above
    d: WeightsData,
    /// Each user's current vote
    votes: LookupMap<AccountId, i128>
}

#[ext_contract(ext_self)]
pub trait Contract {
    fn exchange_callback_post_withdraw(&mut self, sender_id: AccountId, amount: U128);
    fn on_stake_change(&mut self, account: AccountId, #[callback] balances: (Balance, Balance, Balance));
    fn on_vote_change(&mut self, old_vote: i128, new_vote: i128, #[callback] balances: (Balance, Balance));
}

#[near_bindgen]
impl Contract {
    #[init]
    pub fn new(
        owner_id: ValidAccountId,
        token_id: ValidAccountId,
        unstake_period: WrappedDuration,
    ) -> Self {
        Self {
            owner_id: owner_id.into(),
            vote_token_id: token_id.into(),
            users: LookupMap::new(StorageKeys::Users),
            total_amount: 0,
            unstake_period: unstake_period.0,
            median: -1,
            d: WeightsData::new(),
            votes: LookupMap::new(StorageKeys::Votes),
        }
    }

    /// Total number of tokens staked in this contract.
    pub fn ft_total_supply(&self) -> U128 {
        U128(self.total_amount)
    }

    /// Total number of tokens staked by given user.
    pub fn ft_balance_of(&self, account_id: ValidAccountId) -> U128 {
        U128(self.internal_get_user(account_id.as_ref()).vote_amount.0)
    }

    /// Returns user information.
    pub fn get_user(&self, account_id: ValidAccountId) -> User {
        self.internal_get_user(account_id.as_ref())
    }

    /// Delegate give amount of votes to given account.
    /// If enough tokens and storage, forwards this to owner account.
    pub fn delegate(&mut self, account_id: ValidAccountId, amount: U128) -> Promise {
        let sender_id = env::predecessor_account_id();
        self.internal_delegate(sender_id.clone(), account_id.clone().into(), amount.0);
        ext_sputnik::delegate(
            account_id.into(),
            amount,
            &self.owner_id,
            0,
            GAS_FOR_DELEGATE,
        )
        .then(ext_self::on_stake_change(
            sender_id,
            // promise params
            &env::current_account_id(),
            0, TMP_GAS
        ))
    }

    /// Remove given amount of delegation.
    pub fn undelegate(&mut self, account_id: ValidAccountId, amount: U128) -> Promise {
        let sender_id = env::predecessor_account_id();
        self.internal_undelegate(sender_id.clone(), account_id.clone().into(), amount.0);
        ext_sputnik::undelegate(
            account_id.into(),
            amount,
            &self.owner_id,
            0,
            GAS_FOR_UNDELEGATE,
        )
        .then(ext_self::on_stake_change(
            sender_id,
            // promise params
            &env::current_account_id(),
            0, TMP_GAS
        ))
    }

    /// Withdraw non delegated tokens back to the user's account.
    /// If user's account is not registered, will keep funds here.
    pub fn withdraw(&mut self, amount: U128) -> Promise {
        let sender_id = env::predecessor_account_id();
        self.internal_withdraw(&sender_id, amount.0);
        ext_fungible_token::ft_transfer(
            sender_id.clone(),
            amount,
            None,
            &self.vote_token_id,
            1,
            GAS_FOR_FT_TRANSFER,
        )
        .then(ext_self::exchange_callback_post_withdraw(
            sender_id,
            amount,
            &env::current_account_id(),
            0,
            GAS_FOR_FT_TRANSFER,
        ))
    }

    pub fn vote(&mut self, new_vote: i128) -> Promise {
        assert_eq!(env::attached_deposit(), 16 * env::storage_byte_cost());
        assert!(new_vote > 0, "Vote cannot be negative");
        let account = env::predecessor_account_id();
        let old_vote: i128;
        if let Some(vote) = self.votes.get(&account) {  
            old_vote = vote;
        } else {
            old_vote = -1;
        }
        self.votes.insert(&account, &new_vote);
        ext_sputnik::get_delegation_balances(
            account.into(), 
            &self.owner_id, 
            0, TMP_GAS
        )
        .then(ext_self::on_vote_change(
            old_vote, new_vote, 
            // promise params
            &env::current_account_id(),
            0, TMP_GAS
        ))
    }

    #[private]
    pub fn on_stake_change(&mut self, account: AccountId, #[callback] balances: (Balance, Balance, Balance)) {
        assert_eq!(env::predecessor_account_id(), env::current_account_id());
        if let Some(old_vote) = self.votes.get(&account) {  
            self.rebalance(balances.2, balances.1, old_vote, balances.0, old_vote);    
        }
    }

    #[private]
    pub fn on_vote_change(&mut self, old_vote: i128, new_vote: i128, #[callback] balances: (Balance, Balance)) {
        assert_eq!(env::predecessor_account_id(), env::current_account_id());
        self.rebalance(balances.1, balances.0, old_vote, balances.0, new_vote);
    }

    #[private]
    pub fn exchange_callback_post_withdraw(&mut self, sender_id: AccountId, amount: U128) {
        assert_eq!(
            env::promise_results_count(),
            1,
            "ERR_CALLBACK_POST_WITHDRAW_INVALID",
        );
        match env::promise_result(0) {
            PromiseResult::NotReady => unreachable!(),
            PromiseResult::Successful(_) => {}
            PromiseResult::Failed => {
                // This reverts the changes from withdraw function.
                self.internal_deposit(&sender_id, amount.0);
            }
        };
    }

    /*  Weighted Median Algorithm
	 *  Find value of k in range(1, len(Weights)) such that 
	 *  sum(Weights[0:k]) = sum(Weights[k:len(Weights)+1])
	 *  = sum(Weights) / 2
	 *  If there is no such value of k, there must be a value of k 
	 *  in the same range range(1, len(Weights)) such that 
	 *  sum(Weights[0:k]) > sum(Weights) / 2
	*/

    #[private]
    pub fn rebalance(&mut self, 
                    new_total: Balance, 
                    new_stake: Balance, 
                    new_vote: i128, 
                    old_stake: Balance, 
                    old_vote: i128) {
		
        self.d.total = new_total;
        let mut len = self.d.y.len();
		assert!(len == self.d.w.len(), "Wrong Weights Length");	

		let added: bool; 
		match self.d.y.binary_search(&new_vote) {
			Ok(idx) => {
				if new_stake != 0 {
					self.d.w[idx] = self.d.w[idx].saturating_add(new_stake);
				}
				added = false;
			},
			Err(idx) => {
				self.d.y.insert(idx, new_vote);
				self.d.w.insert(idx, new_stake);
				added = true;
				len += 1;
			}
		}
		let mut median = self.median;

		let mid_stake = self.d.total.checked_div(2).unwrap_or_else(|| 0);
		
		if old_vote != -1 && old_stake != 0 { // if not the first time user is voting
			let idx = self.d.y.binary_search(&old_vote).unwrap_or_else(|x| panic!());
			self.d.w[idx] = self.d.w[idx].saturating_sub(old_stake);
			if self.d.w[idx] == 0 {
				self.d.y.remove(idx);
				self.d.w.remove(idx);
				if (idx as u32) >= self.d.k {
					self.d.k -= 1;
				}
				len -= 1;	
			}
		}
		if self.d.total != 0 && mid_stake != 0 {
			if len == 1 || new_vote <= median {
				self.d.sum_w_k = self.d.sum_w_k.saturating_add(new_stake.into());
			}		  
			if old_vote <= median {   
				self.d.sum_w_k = self.d.sum_w_k.saturating_sub(old_stake.into());
			}
			if median > new_vote {
				if added && len > 1 {
					self.d.k += 1;
				}
				while self.d.k >= 1 && ((self.d.sum_w_k.saturating_sub(self.d.w[self.d.k as usize])) >= mid_stake) {
					self.d.sum_w_k = self.d.sum_w_k.saturating_sub(self.d.w[self.d.k as usize]);
					self.d.k -= 1;			
				}
			} else {
				while self.d.sum_w_k < mid_stake {
					self.d.k += 1;
					self.d.sum_w_k = self.d.sum_w_k.saturating_add(self.d.w[self.d.k as usize]);
				}
			}
			median = self.d.y[self.d.k as usize];
			if self.d.sum_w_k == mid_stake {
				let intermedian = median.saturating_add(self.d.y[self.d.k as usize + 1]);
				median = intermedian.checked_div(2).unwrap_or_else(|| median);
			}
		}  else {
			self.d.sum_w_k = 0;
		}
		self.median = median;
    }
}

#[near_bindgen]
impl FungibleTokenReceiver for Contract {
    fn ft_on_transfer(
        &mut self,
        sender_id: ValidAccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        assert_eq!(
            self.vote_token_id,
            env::predecessor_account_id(),
            "ERR_INVALID_TOKEN"
        );
        assert!(msg.is_empty(), "ERR_INVALID_MESSAGE");
        self.internal_deposit(sender_id.as_ref(), amount.0);
        PromiseOrValue::Value(U128(0))
    }
}

#[cfg(test)]
mod tests {
    use near_contract_standards::storage_management::StorageManagement;
    use near_sdk::json_types::U64;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::{testing_env, MockedBlockchain};

    use near_sdk_sim::to_yocto;

    use super::*;

    #[test]
    fn test_basics() {
        let period = 1000;
        let mut context = VMContextBuilder::new();
        testing_env!(context.predecessor_account_id(accounts(0)).build());
        let mut contract = Contract::new(accounts(0), accounts(1), U64(period));
        testing_env!(context.attached_deposit(to_yocto("1")).build());
        contract.storage_deposit(Some(accounts(2)), None);
        testing_env!(context.predecessor_account_id(accounts(1)).build());
        contract.ft_on_transfer(accounts(2), U128(to_yocto("100")), "".to_string());
        assert_eq!(contract.ft_total_supply().0, to_yocto("100"));
        assert_eq!(contract.ft_balance_of(accounts(2)).0, to_yocto("100"));
        testing_env!(context.predecessor_account_id(accounts(2)).build());
        contract.withdraw(U128(to_yocto("50")));
        assert_eq!(contract.ft_total_supply().0, to_yocto("50"));
        assert_eq!(contract.ft_balance_of(accounts(2)).0, to_yocto("50"));
        contract.delegate(accounts(3), U128(to_yocto("10")));
        let user = contract.get_user(accounts(2));
        assert_eq!(user.delegated_amount(), to_yocto("10"));
        contract.undelegate(accounts(3), U128(to_yocto("10")));
        let user = contract.get_user(accounts(2));
        assert_eq!(user.delegated_amount(), 0);
        assert_eq!(user.next_action_timestamp, U64(period));
    }
}
