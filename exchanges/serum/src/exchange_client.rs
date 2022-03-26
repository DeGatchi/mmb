use crate::serum::Serum;
use anyhow::Result;
use async_trait::async_trait;
use function_name::named;
use futures::future::join_all;
use itertools::Itertools;
use serum_dex::matching::Side;
use serum_dex::state::MarketState;
use solana_program::account_info::IntoAccountInfo;
use solana_program::pubkey::Pubkey;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;

use mmb_core::exchanges::common::{
    ActivePosition, ClosedPosition, CurrencyCode, CurrencyPair, ExchangeError, ExchangeErrorType,
    Price,
};
use mmb_core::exchanges::events::ExchangeBalancesAndPositions;
use mmb_core::exchanges::general::exchange::RequestResult;
use mmb_core::exchanges::general::order::cancel::CancelOrderResult;
use mmb_core::exchanges::general::order::create::CreateOrderResult;
use mmb_core::exchanges::general::order::get_order_trades::OrderTrade;
use mmb_core::exchanges::general::symbol::Symbol;
use mmb_core::exchanges::traits::ExchangeClient;
use mmb_core::orders::fill::EventSourceType;
use mmb_core::orders::order::{OrderCancelling, OrderCreating, OrderInfo};
use mmb_core::orders::pool::OrderRef;
use mmb_utils::DateTime;

#[async_trait]
impl ExchangeClient for Serum {
    async fn create_order(&self, order: OrderCreating) -> CreateOrderResult {
        // TODO Possible handle ExchangeError in create_order_core
        match self.create_order_core(order).await {
            Ok(exchange_order_id) => {
                CreateOrderResult::successed(&exchange_order_id, EventSourceType::Rpc)
            }
            Err(error) => CreateOrderResult::failed(
                ExchangeError::new(ExchangeErrorType::Unknown, error.to_string(), None),
                EventSourceType::Rpc,
            ),
        }
    }

    async fn cancel_order(&self, order: OrderCancelling) -> CancelOrderResult {
        // TODO Possible handle ExchangeError in create_order_core
        match self.cancel_order_core(&order).await {
            Ok(_) => CancelOrderResult::successed(
                order.header.client_order_id.clone(),
                EventSourceType::Rpc,
                None,
            ),
            Err(error) => CancelOrderResult::failed(
                ExchangeError::new(ExchangeErrorType::Unknown, error.to_string(), None),
                EventSourceType::Rpc,
            ),
        }
    }

    async fn cancel_all_orders(&self, currency_pair: CurrencyPair) -> Result<()> {
        self.cancel_all_orders_core(&currency_pair).await
    }

    async fn get_open_orders(&self) -> Result<Vec<OrderInfo>> {
        let currency_pairs = self.markets_data.read().keys().cloned().collect_vec();

        join_all(
            currency_pairs
                .into_iter()
                .map(|currency_pair| self.get_open_orders_by_currency_pair(currency_pair)),
        )
        .await
        .into_iter()
        .flatten_ok()
        .collect()
    }

    async fn get_open_orders_by_currency_pair(
        &self,
        currency_pair: CurrencyPair,
    ) -> Result<Vec<OrderInfo>> {
        let market_data = self.get_market_data(&currency_pair)?;
        let program_id = &market_data.program_id;
        let market_metadata = &market_data.metadata;
        let mut account = self
            .rpc_client
            .get_account(&market_metadata.owner_address)?;
        let account_info = (program_id, &mut account).into_account_info();

        let market_data = MarketState::load(&account_info, program_id, false)?;

        let mut asks_account = self.rpc_client.get_account(&market_metadata.asks_address)?;
        let mut bids_account = self.rpc_client.get_account(&market_metadata.bids_address)?;
        let asks_info = (&market_metadata.asks_address, &mut asks_account).into_account_info();
        let bids_info = (&market_metadata.bids_address, &mut bids_account).into_account_info();
        let mut bids = market_data.load_bids_mut(&bids_info)?;
        let mut asks = market_data.load_asks_mut(&asks_info)?;

        let bids_slab = bids.deref_mut();
        let asks_slab = asks.deref_mut();

        let mut orders =
            self.encode_orders(asks_slab, &market_metadata, Side::Ask, &currency_pair)?;
        orders.append(&mut self.encode_orders(
            bids_slab,
            &market_metadata,
            Side::Bid,
            &currency_pair,
        )?);

        Ok(orders)
    }

    async fn get_order_info(&self, order: &OrderRef) -> Result<OrderInfo, ExchangeError> {
        self.do_get_order_info(order).await.map_err(|error| {
            ExchangeError::new(ExchangeErrorType::Unknown, error.to_string(), None)
        })
    }

    async fn close_position(
        &self,
        _position: &ActivePosition,
        _price: Option<Price>,
    ) -> Result<ClosedPosition> {
        todo!()
    }

    async fn get_active_positions(&self) -> Result<Vec<ActivePosition>> {
        todo!()
    }

    async fn get_balance(&self, is_spot: bool) -> Result<ExchangeBalancesAndPositions> {
        if !is_spot {
            // price_mint_address and coin_mint_address are the same for different currency pairs and corresponding CurrencyCode
            let mint_addresses: HashMap<CurrencyCode, Pubkey> = self
                .markets_data
                .read()
                .iter()
                .flat_map(|(pair, market)| {
                    let pair_codes = pair.to_codes();
                    let market_metadata = market.metadata;

                    [
                        (pair_codes.base, market_metadata.price_mint_address),
                        (pair_codes.quote, market_metadata.coin_mint_address),
                    ]
                })
                .collect();

            let balances = join_all(mint_addresses.iter().map(|(currency_code, mint_address)| {
                self.get_exchange_balance_from_account(&currency_code, &mint_address)
            }))
            .await
            .into_iter()
            .try_collect()?;

            Ok(ExchangeBalancesAndPositions {
                balances,
                positions: None,
            })
        } else {
            unimplemented!()
        }
    }

    async fn get_my_trades(
        &self,
        _symbol: &Symbol,
        _last_date_time: Option<DateTime>,
    ) -> Result<RequestResult<Vec<OrderTrade>>> {
        todo!()
    }

    #[named]
    async fn build_all_symbols(&self) -> Result<Vec<Arc<Symbol>>> {
        let request_symbols = self
            .rest_client
            .get(
                self.network_type
                    .market_list_url()
                    .try_into()
                    .expect("Unable create url"),
                "",
                function_name!(),
                "".to_string(),
            )
            .await?;

        self.parse_all_symbols(&request_symbols)
    }
}
