use std::sync::{Arc, Mutex};

use starcoin_config::NodeConfig;
use starcoin_consensus::{FlexiDagStorageConfig, FlexiDagStorage, BlockDAG};
use starcoin_crypto::HashValue;
use starcoin_service_registry::{bus::BusService, RegistryService, RegistryAsyncService};
use starcoin_storage::Store;
use starcoin_sync::block_connector::WriteBlockChainService;
use starcoin_txpool_mock_service::MockTxPoolService;
use starcoin_types::{startup_info::StartupInfo, header::Header, blockhash::ORIGIN};
use starcoin_genesis::Genesis as StarcoinGenesis;



pub async fn create_writeable_block_chain() -> (
    WriteBlockChainService<MockTxPoolService>,
    Arc<NodeConfig>,
    Arc<dyn Store>,
) {
    let node_config = NodeConfig::random_for_test();
    let node_config = Arc::new(node_config);

    let (storage, chain_info, _) = StarcoinGenesis::init_storage_for_test(node_config.net())
        .expect("init storage by genesis fail.");
    let registry = RegistryService::launch();
    let bus = registry.service_ref::<BusService>().await.unwrap();
    let txpool_service = MockTxPoolService::new();

    let (chain_info, genesis) = StarcoinGenesis::init_and_check_storage(
        node_config.net(),
        storage.clone(),
        node_config.data_dir(),
    )
    .expect("init chain and genesis error");

    let flex_dag_config = FlexiDagStorageConfig::create_with_params(1, 0, 1024);
    let flex_dag_db = FlexiDagStorage::create_from_path("./smolstc", flex_dag_config)
        .expect("Failed to create flexidag storage");

    let dag = BlockDAG::new(
        Header::new(
            genesis.block().header().clone(),
            vec![HashValue::new(ORIGIN)],
        ),
        3,
        flex_dag_db,
    );

    (
        WriteBlockChainService::new(
            node_config.clone(),
            StartupInfo::new(chain_info.head().id()),
            storage.clone(),
            txpool_service,
            bus,
            None,
            Arc::new(Mutex::new(dag)),
        )
        .unwrap(),
        node_config,
        storage,
    )
}

fn main() {
    println!("Hello, world!");
}

