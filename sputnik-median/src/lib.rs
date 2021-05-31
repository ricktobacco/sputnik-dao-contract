use near_contract_standards::fungible_token::core_impl::ext_fungible_token;
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};

use near_sdk::json_types::{ValidAccountId, WrappedDuration, U128};
use near_sdk::{
    env, log, ext_contract, near_bindgen, AccountId, Balance, BorshStorageKey, Duration, Gas,
    PanicOnDefault, Promise, PromiseOrValue, PromiseResult,
};

pub const TMP_GAS: Gas = 10_000_000_000_000;

#[ext_contract(ext_sputnik)]
pub trait Sputnik {
    fn delegation_total_supply(&self) -> U128;
    fn delegation_balance_of(&self) -> U128;
    fn delegate(&mut self, account_id: AccountId, amount: U128);
    fn undelegate(&mut self, account_id: AccountId, amount: U128);
}

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
            total: 0,
            k: 0,
            sum_w_k: 0,
            y: Vec::new(),
            w: Vec::new()
        }
    }
}

#[near_bindgen]
#[derive(BorshSerialize, BorshDeserialize, PanicOnDefault)]
pub struct Contract {
    /// DAO owner of this medianizer
    pub sputnik: AccountId,
    pub median: i128, // result of rebalance function
    pub d: WeightsData, // d stands for Data

}

// sputnik.delegation_total_supply
// sputnik.delegation_balance_of

#[near_bindgen]
impl Contract {
    #[init]
    pub fn new(
        id: ValidAccountId
    ) -> Self {
        Self {
            sputnik: id.into(),
            median: -1,
            d: WeightsData::new()
        }
    }
     
    
    pub fn test_xcc (&self) {
        ext_sputnik::delegation_total_supply(
            &self.sputnik,
            0,
            TMP_GAS,
        ).then(|res| {
            log!("Result...{}", &res);    
        });
        
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
        let mut contract = Contract::new(accounts(0));
        contract.test_xcc();
    }
}
