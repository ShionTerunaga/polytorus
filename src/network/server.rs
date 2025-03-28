//! server of Blockchain
use crate::blockchain::block::*;
use crate::blockchain::utxoset::*;
use crate::crypto::fndsa::FnDsaCrypto;
use crate::crypto::transaction::*;
use crate::crypto::wallets::Wallets;
use crate::Result;
use bincode::{deserialize, serialize};
use failure::format_err;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::prelude::*;
use std::net::{TcpListener, TcpStream};
use std::sync::*;
use std::thread;
use std::time::Duration;
use std::vec;

#[derive(Serialize, Deserialize, Debug, Clone)]
enum Message {
    Addr(Vec<String>),
    Version(Versionmsg),
    Tx(Txmsg),
    GetData(GetDatamsg),
    GetBlock(GetBlocksmsg),
    Inv(Invmsg),
    Block(Blockmsg),
    SignRequest(SignRequestMsg),
    SignResponse(SignResponseMsg),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Blockmsg {
    addr_from: String,
    block: Block,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct GetBlocksmsg {
    addr_from: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct GetDatamsg {
    addr_from: String,
    kind: String,
    id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Invmsg {
    addr_from: String,
    kind: String,
    items: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Txmsg {
    addr_from: String,
    transaction: Transaction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Versionmsg {
    addr_from: String,
    version: i32,
    best_height: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SignRequestMsg {
    addr_from: String,
    address: String,
    transaction: Transaction,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SignResponseMsg {
    addr_from: String,
    transaction: Transaction,
    success: bool,
    error_message: String,
}

pub struct Server {
    node_address: String,
    mining_address: String,
    inner: Arc<Mutex<ServerInner>>,
}

struct ServerInner {
    known_nodes: HashSet<String>,
    utxo: UTXOSet,
    blocks_in_transit: Vec<String>,
    mempool: HashMap<String, Transaction>,
}

const CMD_LEN: usize = 12;
const VERSION: i32 = 1;

impl Server {
    pub fn new(
        host: &str,
        port: &str,
        miner_address: &str,
        bootstap: Option<&str>,
        utxo: UTXOSet,
    ) -> Result<Server> {
        let mut node_set = HashSet::new();
        // node_set.insert(String::from(KNOWN_NODE1));
        if let Some(bn) = bootstap {
            node_set.insert(bn.to_string());
        }
        Ok(Server {
            node_address: format!("{}:{}", host, port),
            mining_address: miner_address.to_string(),
            inner: Arc::new(Mutex::new(ServerInner {
                known_nodes: node_set,
                utxo,
                blocks_in_transit: Vec::new(),
                mempool: HashMap::new(),
            })),
        })
    }

    pub fn start_server(&self) -> Result<()> {
        let server1 = Server {
            node_address: self.node_address.clone(),
            mining_address: self.mining_address.clone(),
            inner: Arc::clone(&self.inner),
        };
        info!(
            "Start server at {}, minning address: {}",
            &self.node_address, &self.mining_address
        );

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(1000));
            if server1.get_best_height()? == -1 {
                server1.request_blocks()
            } else {
                let nodes = server1.get_known_nodes();
                if !nodes.is_empty() {
                    let first = nodes.iter().next().unwrap();
                    server1.send_version(first)?;
                };
                Ok(())
            }
        });

        let listener = TcpListener::bind(&self.node_address).unwrap();
        info!("Server listen...");

        for stream in listener.incoming() {
            let stream = stream?;
            let server1 = Server {
                node_address: self.node_address.clone(),
                mining_address: self.mining_address.clone(),
                inner: Arc::clone(&self.inner),
            };
            thread::spawn(move || server1.handle_connection(stream));
        }

        Ok(())
    }

    pub fn send_transaction(tx: &Transaction, utxoset: UTXOSet, target_addr: &str) -> Result<()> {
        let server = Server::new("0.0.0.0", "7000", "", None, utxoset)?;
        server.send_tx(target_addr, tx)?;
        Ok(())
    }

    /* ------------------- inner halp functions ----------------------------------*/

    fn remove_node(&self, addr: &str) {
        self.inner.lock().unwrap().known_nodes.remove(addr);
    }

    fn add_nodes(&self, addr: &str) {
        self.inner
            .lock()
            .unwrap()
            .known_nodes
            .insert(String::from(addr));
    }

    fn get_known_nodes(&self) -> HashSet<String> {
        self.inner.lock().unwrap().known_nodes.clone()
    }

    fn node_is_known(&self, addr: &str) -> bool {
        self.inner.lock().unwrap().known_nodes.get(addr).is_some()
    }

    fn replace_in_transit(&self, hashs: Vec<String>) {
        let bit = &mut self.inner.lock().unwrap().blocks_in_transit;
        bit.clone_from(&hashs);
    }

    fn get_in_transit(&self) -> Vec<String> {
        self.inner.lock().unwrap().blocks_in_transit.clone()
    }

    fn get_mempool_tx(&self, addr: &str) -> Option<Transaction> {
        self.inner.lock().unwrap().mempool.get(addr).cloned()
    }

    fn get_mempool(&self) -> HashMap<String, Transaction> {
        self.inner.lock().unwrap().mempool.clone()
    }

    fn insert_mempool(&self, tx: Transaction) {
        self.inner.lock().unwrap().mempool.insert(tx.id.clone(), tx);
    }

    fn clear_mempool(&self) {
        self.inner.lock().unwrap().mempool.clear()
    }

    fn get_best_height(&self) -> Result<i32> {
        self.inner.lock().unwrap().utxo.blockchain.get_best_height()
    }

    fn get_block_hashs(&self) -> Vec<String> {
        self.inner.lock().unwrap().utxo.blockchain.get_block_hashs()
    }

    fn get_block(&self, block_hash: &str) -> Result<Block> {
        self.inner
            .lock()
            .unwrap()
            .utxo
            .blockchain
            .get_block(block_hash)
    }

    fn verify_tx(&self, tx: &Transaction) -> Result<bool> {
        self.inner
            .lock()
            .unwrap()
            .utxo
            .blockchain
            .verify_transacton(tx)
    }

    fn add_block(&self, block: Block) -> Result<()> {
        self.inner.lock().unwrap().utxo.blockchain.add_block(block)
    }

    fn mine_block(&self, txs: Vec<Transaction>) -> Result<Block> {
        self.inner.lock().unwrap().utxo.blockchain.mine_block(txs)
    }

    fn utxo_reindex(&self) -> Result<()> {
        self.inner.lock().unwrap().utxo.reindex()
    }

    /* -----------------------------------------------------*/

    fn send_data(&self, addr: &str, data: &[u8]) -> Result<()> {
        if addr == &self.node_address {
            return Ok(());
        }
        let mut stream = match TcpStream::connect(addr) {
            Ok(s) => s,
            Err(_) => {
                self.remove_node(addr);
                return Ok(());
            }
        };

        stream.write(data)?;

        info!("data send successfully");
        Ok(())
    }

    fn request_blocks(&self) -> Result<()> {
        for node in self.get_known_nodes() {
            self.send_get_blocks(&node)?
        }
        Ok(())
    }

    fn send_block(&self, addr: &str, b: &Block) -> Result<()> {
        info!("send block data to: {} block hash: {}", addr, b.get_hash());
        let data = Blockmsg {
            addr_from: self.node_address.clone(),
            block: b.clone(),
        };
        let data = serialize(&(cmd_to_bytes("block"), data))?;
        self.send_data(addr, &data)
    }

    fn send_addr(&self, addr: &str) -> Result<()> {
        info!("send address info to: {}", addr);
        let nodes = self.get_known_nodes();
        let data = serialize(&(cmd_to_bytes("addr"), nodes))?;
        self.send_data(addr, &data)
    }

    fn send_inv(&self, addr: &str, kind: &str, items: Vec<String>) -> Result<()> {
        info!(
            "send inv message to: {} kind: {} data: {:?}",
            addr, kind, items
        );
        let data = Invmsg {
            addr_from: self.node_address.clone(),
            kind: kind.to_string(),
            items,
        };
        let data = serialize(&(cmd_to_bytes("inv"), data))?;
        self.send_data(addr, &data)
    }

    fn send_get_blocks(&self, addr: &str) -> Result<()> {
        info!("send get blocks message to: {}", addr);
        let data = GetBlocksmsg {
            addr_from: self.node_address.clone(),
        };
        let data = serialize(&(cmd_to_bytes("getblocks"), data))?;
        self.send_data(addr, &data)
    }

    fn send_get_data(&self, addr: &str, kind: &str, id: &str) -> Result<()> {
        info!(
            "send get data message to: {} kind: {} id: {}",
            addr, kind, id
        );
        let data = GetDatamsg {
            addr_from: self.node_address.clone(),
            kind: kind.to_string(),
            id: id.to_string(),
        };
        let data = serialize(&(cmd_to_bytes("getdata"), data))?;
        self.send_data(addr, &data)
    }

    pub fn send_tx(&self, addr: &str, tx: &Transaction) -> Result<()> {
        info!("send tx to: {} txid: {}", addr, &tx.id);
        let data = Txmsg {
            addr_from: self.node_address.clone(),
            transaction: tx.clone(),
        };
        let data = serialize(&(cmd_to_bytes("tx"), data))?;
        self.send_data(addr, &data)
    }

    fn send_version(&self, addr: &str) -> Result<()> {
        info!("send version info to: {}", addr);
        let data = Versionmsg {
            addr_from: self.node_address.clone(),
            best_height: self.get_best_height()?,
            version: VERSION,
        };
        let data = serialize(&(cmd_to_bytes("version"), data))?;
        self.send_data(addr, &data)
    }

    fn handle_version(&self, msg: Versionmsg) -> Result<()> {
        info!("receive version msg: {:#?}", msg);
        let my_best_height = self.get_best_height()?;
        if my_best_height < msg.best_height {
            self.send_get_blocks(&msg.addr_from)?;
        } else if my_best_height > msg.best_height {
            self.send_version(&msg.addr_from)?;
        }

        self.send_addr(&msg.addr_from)?;

        if !self.node_is_known(&msg.addr_from) {
            self.add_nodes(&msg.addr_from);
        }
        Ok(())
    }

    fn handle_addr(&self, msg: Vec<String>) -> Result<()> {
        info!("receive address msg: {:#?}", msg);
        for node in msg {
            self.add_nodes(&node);
        }
        //self.request_blocks()?;
        Ok(())
    }

    fn handle_block(&self, msg: Blockmsg) -> Result<()> {
        info!(
            "receive block msg: {}, {}",
            msg.addr_from,
            msg.block.get_hash()
        );
        self.add_block(msg.block)?;

        let mut in_transit = self.get_in_transit();
        if !in_transit.is_empty() {
            let block_hash = &in_transit[0];
            self.send_get_data(&msg.addr_from, "block", block_hash)?;
            in_transit.remove(0);
            self.replace_in_transit(in_transit);
        } else {
            self.utxo_reindex()?;
        }

        Ok(())
    }

    fn handle_inv(&self, msg: Invmsg) -> Result<()> {
        info!("receive inv msg: {:#?}", msg);
        if msg.kind == "block" {
            let block_hash = &msg.items[0];
            self.send_get_data(&msg.addr_from, "block", block_hash)?;

            let mut new_in_transit = Vec::new();
            for b in &msg.items {
                if b != block_hash {
                    new_in_transit.push(b.clone());
                }
            }
            self.replace_in_transit(new_in_transit);
        } else if msg.kind == "tx" {
            let txid = &msg.items[0];
            match self.get_mempool_tx(txid) {
                Some(tx) => {
                    if tx.id.is_empty() {
                        self.send_get_data(&msg.addr_from, "tx", txid)?
                    }
                }
                None => self.send_get_data(&msg.addr_from, "tx", txid)?,
            }
        }
        Ok(())
    }

    fn handle_get_blocks(&self, msg: GetBlocksmsg) -> Result<()> {
        info!("receive get blocks msg: {:#?}", msg);
        let block_hashs = self.get_block_hashs();
        self.send_inv(&msg.addr_from, "block", block_hashs)?;
        Ok(())
    }

    fn handle_get_data(&self, msg: GetDatamsg) -> Result<()> {
        info!("receive get data msg: {:#?}", msg);
        if msg.kind == "block" {
            let block = self.get_block(&msg.id)?;
            self.send_block(&msg.addr_from, &block)?;
        } else if msg.kind == "tx" {
            let tx = self.get_mempool_tx(&msg.id).unwrap();
            self.send_tx(&msg.addr_from, &tx)?;
        }
        Ok(())
    }

    fn handle_tx(&self, msg: Txmsg) -> Result<()> {
        info!("receive tx msg: {} {}", msg.addr_from, &msg.transaction.id);
        self.insert_mempool(msg.transaction.clone());

        let known_nodes = self.get_known_nodes();

        for node in known_nodes {
            if node != self.node_address && node != msg.addr_from {
                self.send_inv(&node, "tx", vec![msg.transaction.id.clone()])?;
            }
        }

        if !self.mining_address.is_empty() {
            let mut mempool = self.get_mempool();
            debug!("Current mempool: {:#?}", &mempool);

            if !mempool.is_empty() {
                loop {
                    let mut txs = Vec::new();

                    for tx in mempool.values() {
                        if self.verify_tx(tx)? {
                            txs.push(tx.clone());
                        }
                    }

                    if txs.is_empty() {
                        return Ok(());
                    }

                    let cbtx =
                        Transaction::new_coinbase(self.mining_address.clone(), String::new())?;
                    txs.push(cbtx);

                    for tx in &txs {
                        mempool.remove(&tx.id);
                    }

                    let new_block = self.mine_block(txs)?;
                    self.utxo_reindex()?;

                    for node in self.get_known_nodes() {
                        if node != self.node_address {
                            self.send_inv(&node, "block", vec![new_block.get_hash()])?;
                        }
                    }

                    if mempool.is_empty() {
                        break;
                    }
                }
                self.clear_mempool();
            }
        }

        Ok(())
    }

    pub fn send_sign_request(
        &self,
        addr: &str,
        wallet_addr: &str,
        tx: &Transaction,
    ) -> Result<Transaction> {
        info!("send sign request to: {} for wallet: {}", addr, wallet_addr);
        let data = SignRequestMsg {
            addr_from: self.node_address.clone(),
            address: wallet_addr.to_string(),
            transaction: tx.clone(),
        };
        let data = serialize(&(cmd_to_bytes("signreq"), data))?;

        let mut stream = match TcpStream::connect(addr) {
            Ok(s) => s,
            Err(e) => {
                error!("Connection failed: {}", e);
                self.remove_node(addr);
                return Err(format_err!("Connection failed: {}", e));
            }
        };

        stream.set_read_timeout(Some(Duration::from_secs(30)))?;

        info!("Writing request data: {} bytes", data.len());

        stream.write_all(&data)?;
        stream.flush()?;

        let mut buffer = vec![0; 10240];
        info!("Waiting for response...");
        let count = stream.read(&mut buffer)?;
        buffer.truncate(count);

        info!("Received response: {} bytes", buffer.len());

        if count == 0 {
            return Err(format_err!("Empty response from server"));
        }

        match bytes_to_cmd(&buffer)? {
            Message::SignResponse(res) => {
                if res.success {
                    Ok(res.transaction)
                } else {
                    Err(format_err!(
                        "Transaction sign failed: {}",
                        res.error_message
                    ))
                }
            }
            _ => Err(format_err!("Unexpected response from server")),
        }
    }

    fn handle_sign_request(&self, msg: SignRequestMsg) -> Result<()> {
        info!(
            "receive sign request from: {} for wallet: {}",
            msg.addr_from, msg.address
        );

        let wallets = Wallets::new()?;
        let wallet = match wallets.get_wallet(&msg.address) {
            Some(w) => w,
            None => {
                let response = SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: msg.transaction.clone(),
                    success: false,
                    error_message: format!("Wallet not found: {}", msg.address),
                };
                let data = serialize(&(cmd_to_bytes("signres"), response))?;
                self.send_data(&msg.addr_from, &data)?;
                return Ok(());
            }
        };

        let mut tx = msg.transaction.clone();
        let crypto = FnDsaCrypto;

        match self.inner.lock().unwrap().utxo.blockchain.sign_transacton(
            &mut tx,
            &wallet.secret_key,
            &crypto,
        ) {
            Ok(_) => {
                // 署名成功
                let response = SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: tx,
                    success: true,
                    error_message: String::new(),
                };
                let data = serialize(&(cmd_to_bytes("signres"), response))?;
                self.send_data(&msg.addr_from, &data)?;
            }
            Err(e) => {
                // 署名失敗
                let response = SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: msg.transaction,
                    success: false,
                    error_message: format!("Signing error: {}", e),
                };
                let data = serialize(&(cmd_to_bytes("signres"), response))?;
                self.send_data(&msg.addr_from, &data)?;
            }
        }

        Ok(())
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        info!("Accepting connection from {:?}", stream.peer_addr()?);

        let mut buffer = vec![0; 4096];
        let count = stream.read_to_end(&mut buffer)?;
        buffer.truncate(count);

        info!("Accept request: length {}", count);

        let cmd = bytes_to_cmd(&buffer)?;

        match cmd {
            Message::Addr(data) => self.handle_addr(data)?,
            Message::Block(data) => self.handle_block(data)?,
            Message::Inv(data) => self.handle_inv(data)?,
            Message::GetBlock(data) => self.handle_get_blocks(data)?,
            Message::GetData(data) => self.handle_get_data(data)?,
            Message::Tx(data) => self.handle_tx(data)?,
            Message::Version(data) => self.handle_version(data)?,
            Message::SignRequest(data) => {
                info!("Processing sign request from: {}", data.addr_from);
                let response = self.prepare_sign_response(data)?;
                let response_data = serialize(&(cmd_to_bytes("signres"), response))?;

                info!("Sending response: size {}", response_data.len());
                stream.write_all(&response_data)?;
                stream.flush()?;

                drop(stream);
            }
            Message::SignResponse(_) => {}
        }

        Ok(())
    }

    pub fn prepare_sign_response(&self, msg: SignRequestMsg) -> Result<SignResponseMsg> {
        info!(
            "receive sign request from: {} for wallet: {}",
            msg.addr_from, msg.address
        );

        let wallets = Wallets::new()?;
        let wallet = match wallets.get_wallet(&msg.address) {
            Some(w) => w,
            None => {
                return Ok(SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: msg.transaction.clone(),
                    success: false,
                    error_message: format!("Wallet not found: {}", msg.address),
                });
            }
        };

        let mut tx = msg.transaction.clone();
        let crypto = FnDsaCrypto;

        match self.inner.lock().unwrap().utxo.blockchain.sign_transacton(
            &mut tx,
            &wallet.secret_key,
            &crypto,
        ) {
            Ok(_) => {
                info!("Transaction signed successfully for wallet {}", msg.address);

                Ok(SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: tx,
                    success: true,
                    error_message: String::new(),
                })
            }

            Err(e) => {
                info!(
                    "Transaction signing failed for wallet {}: {}",
                    msg.address, e
                );

                Ok(SignResponseMsg {
                    addr_from: self.node_address.clone(),
                    transaction: msg.transaction,
                    success: false,
                    error_message: format!("Signing error: {}", e),
                })
            }
        }
    }
}

fn cmd_to_bytes(cmd: &str) -> [u8; CMD_LEN] {
    let mut data = [0; CMD_LEN];
    for (i, d) in cmd.as_bytes().iter().enumerate() {
        data[i] = *d;
    }
    data
}

fn bytes_to_cmd(bytes: &[u8]) -> Result<Message> {
    let mut cmd = Vec::new();
    let cmd_bytes = &bytes[..CMD_LEN];
    let data = &bytes[CMD_LEN..];
    for b in cmd_bytes {
        if 0_u8 != *b {
            cmd.push(*b);
        }
    }
    info!("cmd: {}", String::from_utf8(cmd.clone())?);

    if cmd == "addr".as_bytes() {
        let data: Vec<String> = deserialize(data)?;
        Ok(Message::Addr(data))
    } else if cmd == "block".as_bytes() {
        let data: Blockmsg = deserialize(data)?;
        Ok(Message::Block(data))
    } else if cmd == "inv".as_bytes() {
        let data: Invmsg = deserialize(data)?;
        Ok(Message::Inv(data))
    } else if cmd == "getblocks".as_bytes() {
        let data: GetBlocksmsg = deserialize(data)?;
        Ok(Message::GetBlock(data))
    } else if cmd == "getdata".as_bytes() {
        let data: GetDatamsg = deserialize(data)?;
        Ok(Message::GetData(data))
    } else if cmd == "tx".as_bytes() {
        let data: Txmsg = deserialize(data)?;
        Ok(Message::Tx(data))
    } else if cmd == "version".as_bytes() {
        let data: Versionmsg = deserialize(data)?;
        Ok(Message::Version(data))
    } else if cmd == "signreq".as_bytes() {
        let data: SignRequestMsg = deserialize(data)?;
        Ok(Message::SignRequest(data))
    } else if cmd == "signres".as_bytes() {
        let data: SignResponseMsg = deserialize(data)?;
        Ok(Message::SignResponse(data))
    } else {
        Err(format_err!("Unknown command in the server"))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{blockchain::blockchain::*, crypto::types::EncryptionType};
    // use crate::crypto::wallets::*;

    #[test]
    fn test_cmd() {
        let mut ws = Wallets::new().unwrap();
        let wa1 = ws.create_wallet(EncryptionType::FNDSA);
        let bc = Blockchain::create_blockchain(wa1).unwrap();
        let utxo_set = UTXOSet { blockchain: bc };
        let server = Server::new("localhost", "7878", "", None, utxo_set).unwrap();

        let vmsg = Versionmsg {
            addr_from: server.node_address.clone(),
            best_height: server.get_best_height().unwrap(),
            version: VERSION,
        };
        let data = serialize(&(cmd_to_bytes("version"), vmsg.clone())).unwrap();
        if let Message::Version(v) = bytes_to_cmd(&data).unwrap() {
            assert_eq!(v, vmsg);
        } else {
            panic!("wrong!");
        }
    }
}
