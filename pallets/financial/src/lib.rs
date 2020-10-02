#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{decl_module, decl_storage, decl_event, decl_error, ensure};
use frame_support::traits::{Get, UnixTime};
use frame_support::codec::{Codec, Decode, Encode};
use frame_support::dispatch::DispatchError;
use crate::common::{Asset, OnPriceSet};
use crate::capvec::CapVec;
use core::time::Duration;
use sp_std::prelude::Vec;
use sp_std::convert::TryInto;
use sp_std::iter::Iterator;
use sp_std::vec;
use substrate_fixed::transcendental::{ln};
use substrate_fixed::traits::{Fixed, FixedSigned, ToFixed};
use substrate_fixed::types::I9F23;
use core::ops::{AddAssign, BitOrAssign, ShlAssign};

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

pub mod common;
pub mod capvec;

// Type of constants for transcendental operations declared in substrate_fixed crate
type ConstType = I9F23;

pub trait Trait: frame_system::Trait {
	type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;
	type UnixTime: UnixTime;
	type PriceCount: Get<u32>;
	type PricePeriod: Get<u32>;
	type FixedNumberBits: Copy + ToFixed + AddAssign + BitOrAssign + ShlAssign;
	type FixedNumber: Clone + Copy + Codec + FixedSigned<Bits = Self::FixedNumberBits> + PartialOrd<ConstType> + From<ConstType>;
	type Price: Clone + From<Self::FixedNumber> + Into<Self::FixedNumber>;
}

#[derive(Encode, Decode, Clone, Default)]
pub struct PriceUpdate<P> {
	period_start: Duration,
	time: Duration,
	price: P,
}

decl_storage! {
	trait Store for Module<T: Trait> as TemplateModule {
		Updates get(fn updates): map hasher(blake2_128_concat) Asset => Option<PriceUpdate<T::FixedNumber>>;
		Prices get(fn prices): map hasher(blake2_128_concat) Asset => CapVec<T::FixedNumber>;
	}

	add_extra_genesis {
		config(prices): Vec<(Asset, Vec<T::Price>)>;

		build(|config| {
			let price_count = T::PriceCount::get();

			for (asset, values) in config.prices.iter() {
				let mut prices = CapVec::<T::FixedNumber>::new(price_count);

				for v in values.iter() {
					prices.push(v.clone().into());
				}

				Prices::<T>::insert(asset, prices);
			}
		});
	}
}

decl_event!(
	pub enum Event<T> where AccountId = <T as frame_system::Trait>::AccountId {
		SomethingStored(u32, AccountId),
	}
);

decl_error! {
	pub enum Error for Module<T: Trait> {
		PeriodIsInThePast,
		NoPeriodStarted,
		Overflow,
		DivisionByZero,
		NotEnoughPoints,
		InvalidAsset,
		NotImplemented,
		InvalidStorage,
		InvalidPeriodStart,
		Transcendental,
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		type Error = Error<T>;

		fn deposit_event() = default;

		const PriceCount: u32 = T::PriceCount::get();
		const PricePeriod: u32 = T::PricePeriod::get();
	}
}

#[derive(Debug, Eq, PartialEq)]
enum PeriodAction {
	RemainsUnchanged,
	StartedNew(u32),
}

#[derive(Debug, Eq, PartialEq)]
struct PeriodChange {
	period_start: Duration,
	action: PeriodAction,
}

#[derive(Debug, Eq, PartialEq)]
enum PricePeriodError {
	DivisionByZero,
	Overflow,
	PeriodIsInThePast,
	InvalidPeriodStart,
}

struct PricePeriod(u32);

impl PricePeriod {
	fn get_period_id(&self, now: Duration) -> Result<u64, PricePeriodError> {
		let seconds = now.as_secs();
		let period = self.0 as u64;

		seconds.checked_div(60).and_then(|x| x.checked_div(period)).ok_or(PricePeriodError::DivisionByZero)
	}

	fn get_period_id_start(&self, period_id: u64) -> Result<Duration, PricePeriodError> {
		let period = self.0 as u64;
		let seconds: Result<_, PricePeriodError> = period_id.checked_mul(60).and_then(|x| x.checked_mul(period)).ok_or(PricePeriodError::Overflow);

		Ok(Duration::from_secs(seconds?))
	}

	fn get_period_start(&self, now: Duration) -> Result<Duration, PricePeriodError> {
		self.get_period_id_start(self.get_period_id(now)?)
	}

	fn get_curr_period_info(&self, prev_start: Duration, now: Duration) -> Result<(Duration, i32), PricePeriodError> {
		let prev_period_id = self.get_period_id(prev_start)?;
		let curr_period_id = self.get_period_id(now)?;

		let prev: i32 = prev_period_id.try_into().map_err(|_| PricePeriodError::Overflow)?;
		let curr: i32 = curr_period_id.try_into().map_err(|_| PricePeriodError::Overflow)?;
		let delta: Result<_, PricePeriodError> = curr.checked_sub(prev).ok_or(PricePeriodError::Overflow);
		Ok((self.get_period_id_start(curr_period_id)?, delta?))
	}

	fn get_period_change(&self, period_start: Option<Duration>, now: Duration) -> Result<PeriodChange, PricePeriodError> {
		if let Some(period_start) = period_start {
			ensure!(period_start == self.get_period_start(period_start)?, PricePeriodError::InvalidPeriodStart);
		}

		match period_start {
			None => {
				let period_start = self.get_period_start(now)?;

				Ok(PeriodChange {
					period_start,
					action: PeriodAction::StartedNew(0),
				})
			},
			Some(last_start) => {
				let (current_start, periods_elapsed) = self.get_curr_period_info(last_start, now)?;

				if periods_elapsed < 0 {
					// current period is in the past

					Err(PricePeriodError::PeriodIsInThePast)
				} else if periods_elapsed == 0 {
					// period is not changed

					Ok(PeriodChange {
						period_start: last_start,
						action: PeriodAction::RemainsUnchanged,
					})
				} else {
					// period is changed
					
					let empty_periods = (periods_elapsed - 1) as u32;

					Ok(PeriodChange {
						period_start: current_start,
						action: PeriodAction::StartedNew(empty_periods),
					})
				}
			}
		}
	}
}

impl<T: Trait> From<PricePeriodError> for Error<T> {
	fn from(error: PricePeriodError) -> Self {
		match error {
			PricePeriodError::DivisionByZero => Error::<T>::DivisionByZero,
			PricePeriodError::Overflow => Error::<T>::Overflow,
			PricePeriodError::PeriodIsInThePast => Error::<T>::PeriodIsInThePast,
			PricePeriodError::InvalidPeriodStart => Error::<T>::InvalidPeriodStart,
		}
	}
}

#[derive(Debug, Eq, PartialEq)]
enum GetNewPricesError {
	Overflow,
}

fn get_new_prices<P: Clone>(last_price: Option<P>, new_price: P, empty_periods: u32, max_periods: u32) -> Result<Vec<P>, GetNewPricesError> {
	match last_price {
		None => {
			Ok(vec![new_price])
		},
		Some(last_price) => {
			let mut new_prices: Vec<P> = Vec::new();

			let empty_periods = if empty_periods < max_periods {
				empty_periods
			} else {
				max_periods.checked_sub(1).ok_or(GetNewPricesError::Overflow)?
			};

			for _ in 0..empty_periods {
				new_prices.push(last_price.clone());
			}

			new_prices.push(new_price);

			Ok(new_prices)
		}
	}
}

impl<T: Trait> From<GetNewPricesError> for Error<T> {
	fn from(error: GetNewPricesError) -> Self {
		match error {
			GetNewPricesError::Overflow => Error::<T>::Overflow,
		}
	}
}

impl<T: Trait> OnPriceSet for Module<T> {
	type Price = T::Price;

	fn on_price_set(asset: Asset, value: T::Price) -> Result<(), DispatchError> {
		let value: T::FixedNumber = value.into();
		let now = T::UnixTime::now();
		let price_count = T::PriceCount::get();

		let update = Self::updates(asset);
		let prices = Self::prices(asset);

		ensure!(prices.len_cap() == 0 || prices.len_cap() == price_count, Error::<T>::InvalidStorage);
		let init_prices = prices.len_cap() == 0;

		let period_start = update.map(|x| x.period_start);
		let price_period = PricePeriod(T::PricePeriod::get());
		let period_change = price_period.get_period_change(period_start, now).map_err(Into::<Error<T>>::into)?;

		let period_start = period_change.period_start;

		match period_change.action {
			PeriodAction::RemainsUnchanged => {
				Updates::<T>::mutate(&asset, |update| {
					*update = Some(PriceUpdate {
						period_start: period_start,
						time: now,
						price: value,
					});
				});
			},
			PeriodAction::StartedNew(empty_periods) => {
				let new_prices = get_new_prices(prices.last().cloned(), value, empty_periods, price_count).map_err(Into::<Error<T>>::into)?;

				Updates::<T>::mutate(&asset, |update| {
					*update = Some(PriceUpdate {
						period_start: period_start,
						time: now,
						price: value,
					})
				});
				Prices::<T>::mutate(&asset, |prices| {
					if init_prices {
						*prices = CapVec::<T::FixedNumber>::new(price_count);
					}

					for p in new_prices {
						prices.push(p);
					}
				});
			}
		}

		Ok(())
	}
}

pub enum CalcReturnType {
	Linear,
	Log,
}

pub enum CalcVolatilityType {
	Regular,
	Exponential,
}

pub enum CalcCorrelationType {
	Regular,
	Exponential,
}

pub trait Financial {
	type Price;
	type AccountId;

	fn calc_return(return_type: CalcReturnType, asset: Asset) -> Result<Vec<Self::Price>, DispatchError>;
	fn calc_vol(volatility_type: CalcVolatilityType, asset: Asset, ewma_length: u32, return_type: CalcReturnType) -> Result<Self::Price, DispatchError>;
	fn calc_corr(asset1: Asset, asset2: Asset, corr_type: CalcCorrelationType, ewma_length: u32, return_type: CalcReturnType) -> Result<Self::Price, DispatchError>;
	fn calc_portf_vol(account_id: Self::AccountId) -> Result<Self::Price, DispatchError>;
	fn calc_portf_var(account_id: Self::AccountId, return_type: CalcReturnType, z_score: u32) -> Result<Self::Price, DispatchError>;
}

#[derive(Debug, Eq, PartialEq)]
enum MathError {
	Overflow,
	DivisionByZero,
	Transcendental,
}

fn calc_return<F: Fixed>(x1: F, x2: F) -> Result<F, MathError> {
	let ratio = x2.checked_div(x1).ok_or(MathError::DivisionByZero)?;

	ratio.checked_sub(F::from_num(1)).ok_or(MathError::Overflow)
}

fn calc_log_return<F>(x1: F, x2: F) -> Result<F, MathError>
where
	F: FixedSigned + PartialOrd<ConstType> + From<ConstType>,
	F::Bits: Copy + ToFixed + AddAssign + BitOrAssign + ShlAssign,
{
	let ratio = x2.checked_div(x1).ok_or(MathError::DivisionByZero)?;
	ln(ratio).map_err(|_| MathError::Transcendental)
}

impl From<MathError> for CalcReturnError {
	fn from(error: MathError) -> Self {
		match error {
			MathError::DivisionByZero => CalcReturnError::DivisionByZero,
			MathError::Overflow => CalcReturnError::Overflow,
			MathError::Transcendental => CalcReturnError::Transcendental,
		}
	}
}

#[derive(Debug, Eq, PartialEq)]
enum CalcReturnError {
	NotEnoughPoints,
	Overflow,
	DivisionByZero,
	Transcendental,
}

fn calc_return_vec<S, D>(return_type: CalcReturnType, prices: Vec<S>) -> Result<Vec<D>, CalcReturnError>
where
	S: FixedSigned + PartialOrd<ConstType> + From<ConstType>,
	S::Bits: Copy + ToFixed + AddAssign + BitOrAssign + ShlAssign,
	D: From<S>,
{
	let return_func = match return_type {
		CalcReturnType::Linear => calc_return,
		CalcReturnType::Log => calc_log_return,
	};

	let mut result: Vec<D> = Vec::new();

	ensure!(prices.len() > 1, CalcReturnError::NotEnoughPoints);

	for i in 1..prices.len() {
		result.push(return_func(prices[i - 1], prices[i])?.into());
	}

	Ok(result)
}

impl<T: Trait> From<CalcReturnError> for Error<T> {
	fn from(error: CalcReturnError) -> Self {
		match error {
			CalcReturnError::NotEnoughPoints => Error::<T>::NotEnoughPoints,
			CalcReturnError::Overflow => Error::<T>::Overflow,
			CalcReturnError::DivisionByZero => Error::<T>::DivisionByZero,
			CalcReturnError::Transcendental => Error::<T>::Transcendental,
		}
	}
}

impl<T: Trait> Financial for Module<T> {
	type Price = T::Price;
	type AccountId = <T as frame_system::Trait>::AccountId;

	fn calc_return(return_type: CalcReturnType, asset: Asset) -> Result<Vec<T::Price>, DispatchError> {
		let prices: Vec<_> = Prices::<T>::get(asset).iter().cloned().collect();

		let result = calc_return_vec(return_type, prices).map_err(Into::<Error<T>>::into)?;

		Ok(result)
	}

	fn calc_vol(_volatility_type: CalcVolatilityType, _asset: Asset, _ewma_length: u32, _return_type: CalcReturnType) -> Result<Self::Price, DispatchError> {
		Err(Error::<T>::NotImplemented.into())
	}

	fn calc_corr(_asset1: Asset, _asset2: Asset, _corr_type: CalcCorrelationType, _ewma_length: u32, _return_type: CalcReturnType) -> Result<Self::Price, DispatchError> {
		Err(Error::<T>::NotImplemented.into())
	}

	fn calc_portf_vol(_account_id: Self::AccountId) -> Result<Self::Price, DispatchError> {
		Err(Error::<T>::NotImplemented.into())
	}

	fn calc_portf_var(_account_id: Self::AccountId, _return_type: CalcReturnType, _z_score: u32) -> Result<Self::Price, DispatchError> {
		Err(Error::<T>::NotImplemented.into())
	}
}