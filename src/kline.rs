pub mod kline {
    extern crate binance;

    use binance::websockets::*;
    use std::sync::atomic::AtomicBool;
    use self::binance::model::{KlineEvent, KlineSummaries, Kline};
    use log::{info, trace, warn};
    use rusqlite::{params, Connection, Result, Statement};
    use binance::market::*;
    use self::binance::api::Binance;
    use self::binance::errors::Error;
    use crate::strategy::strategy;
    use self::binance::account::Account;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use crate::wallet::wallet::Wallet;
    use crate::db::db::*;
    use crate::bootstrap::Bootstrap;
    use crate::db::db::trade::{create_trades_table, get_trades, delete_trade};
    use crate::db::db::kline::{get_latest_klines, create_kline};
    use crate::db::db::wallet::{get_wallets};
    use crate::strategy::strategy::get_quarantine_bars;


    pub fn handle_kline_event(boot: &Bootstrap, kline_event: KlineEvent, kline_conn: &Connection, wallet_conn: &Connection, trade_conn: &Connection) {
        let kline = &kline_event.kline;
        create_kline(kline_conn, kline);
        // this means it's a "real" event, not from fillup and we should act
        if kline.symbol != "" {
            let config = &boot.config;
            let klines = get_latest_klines(kline_conn);
            // let quarantine_bars = get_quarantine_bars(&trade_conn, &config);
            // 20 minutes quarantine interval, with a max of 10 trades per 20 min (so that means we're buying for 50% of the time)
            // if quarantine_bars.len() > config.quarantine_amount_trades {
            //     info!("Quarantine for {:?} {:?}", kline.symbol, quarantine_bars.len());
            //     return;
            // }
            let (should_sell, should_buy) = strategy::calculate(&klines, &trade_conn, &boot);
            let account: Account = Binance::new(Option::from(config.api_key.key.clone()), Option::from(config.api_key.secret.clone()));

            let quote_order_qty = config.stake_amount;

            if get_trades(&trade_conn, &kline, true).len() < config.max_amount_of_trades_per_bar && get_trades(&trade_conn, &kline, false).len() < config.max_amount_of_trades {
                let wallets = get_wallets(&wallet_conn);
                let wallet = &wallets[0];
                // we're out of $$$ to buy, lets stop
                if wallet.balance.parse::<f64>().unwrap() < config.min_leftover {
                    info!("didn't buy because wallet balance is too low (this could be just because we have a lot of trades open too)")
                } else if should_buy {
                    // 11 USDT
                    match account.market_buy::<&str, f64>(&kline.symbol, quote_order_qty) {
                        Ok(answer) => {
                            info!("Bought {} at {}, amount: {}", &kline.symbol, answer.price, answer.executed_qty);
                            trade_conn.execute(
                                "INSERT INTO trades (id, amount_crypto, amount_money, start_bar_time) VALUES (?1, ?2, ?3, ?4)",
                                params![answer.order_id as i64, answer.executed_qty, quote_order_qty, &kline.start_time],
                            ).unwrap();
                        }
                        Err(e) => warn!("Error: {:?}", e),
                    }
                }
            }

            for trade in get_trades(&trade_conn, &kline, false) {
                let qty = trade.amount_crypto.parse::<f64>().unwrap();
                let diff = (&kline.close.parse::<f64>().unwrap() * qty - quote_order_qty);
                info!("diff for trade: {:?}", diff);
                // -----------
                //
                // TODO re-enable selling is handled by the 1% profit calculator now, running in a separate thread launched from main.rs.
                //
                // -----------
                // TODO switch back to profit calculator (better returns) and implement trailing profit(stop) loss
                // if we get .015% profit we exit the trade
                info!("mode={} {}", config.mode, config.mode == "spot");
                if config.mode == "spot" {
                    if diff > 0.0 && should_sell == true {
                        match account.market_sell::<&str, f64>(&kline.symbol, qty) {
                            Ok(e) => {
                                delete_trade(&trade_conn, trade.id);
                                info!("Sold crypto at profit of, {:?} USDT, {:?}", diff, e)
                            }
                            Err(e) => warn!("Couldn't sell because error: {:?}", e)
                        }
                    }
                }


                // Guard for extreme periods
                if diff <= -(quote_order_qty * 0.25) {
                    // sell, we have made loss at -5% stoploss
                    match account.market_sell::<&str, f64>(&kline.symbol, qty) {
                        Ok(e) => {
                            delete_trade(&trade_conn, trade.id);
                            info!("Sold crypto at 25% LOSS, {:?}", e)
                        }
                        Err(e) => warn!("Couldn't sell because error: {:?}", e)
                    }
                }
            }
        }
    }

    pub fn kline_data_fillup(boot: &Bootstrap, symbol: &String, kline_conn: &Connection, wallet_conn: &Connection, trade_conn: &Connection) {
        info!("Doing data fillup of past 500 klines");
        let market: Market = Binance::new(None, None);
        match market.get_klines(symbol, &boot.config.timeframe, 500, None, None) {
            Ok(kline_summaries) => {
                match kline_summaries {
                    KlineSummaries::AllKlineSummaries(klines) => {
                        for kline in klines {
                            let k = KlineEvent {
                                event_type: "".to_string(),
                                event_time: 0,
                                symbol: "".to_string(),
                                kline: Kline {
                                    start_time: kline.open_time,
                                    end_time: kline.close_time,
                                    symbol: "".to_string(),
                                    interval: "".to_string(),
                                    first_trade_id: 0,
                                    last_trade_id: 0,
                                    open: kline.open.to_string(),
                                    close: kline.close.to_string(),
                                    high: kline.high.to_string(),
                                    low: kline.low.to_string(),
                                    volume: kline.volume.to_string(),
                                    number_of_trades: 0,
                                    is_final_bar: false,
                                    quote_volume: kline.quote_asset_volume.to_string(),
                                    active_buy_volume: "".to_string(),
                                    active_volume_buy_quote: "".to_string(),
                                    ignore_me: "".to_string(),
                                },
                            };
                            handle_kline_event(&boot, k, &kline_conn, &wallet_conn, &trade_conn);
                        }
                    }
                }
            }
            Err(error) => warn!("Could not get past klines {}", error)
        };
        info!("Successfully downloaded and saved all needed past klines")
    }

    pub fn open_kline_stream(boot: &Bootstrap, symbol: String, kline_conn: Connection, wallet_conn: Connection, trade_conn: Connection) {
        create_trades_table(&trade_conn);
        kline_data_fillup(&boot, &symbol, &kline_conn, &wallet_conn, &trade_conn);
        let kline: String = format!("{}", format!("{}@kline_{}", symbol.to_lowercase(), &boot.config.timeframe));
        let mut web_socket: WebSockets = WebSockets::new(|event: WebsocketEvent| {
            match event {
                WebsocketEvent::Kline(kline_event) => handle_kline_event(&boot, kline_event, &kline_conn, &wallet_conn, &trade_conn),
                _ => ()
            };
            Ok(())
        });
        web_socket.connect(&kline).unwrap();
        if let Err(e) = web_socket.event_loop(&AtomicBool::new(true)) {
            match e {
                err => {
                    panic!("Error with websocket event loop {}", err);
                }
            }
        }
        web_socket.disconnect().unwrap();
    }
}
