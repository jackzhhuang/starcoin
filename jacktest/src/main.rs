use std::sync::{Arc, Mutex};

use starcoin_account_api::AccountInfo;
use starcoin_chain_api::{ChainReader, WriteableChainService};
use starcoin_config::{NodeConfig, TimeService};
use starcoin_consensus::{BlockDAG, Consensus, FlexiDagStorage, FlexiDagStorageConfig};
use starcoin_crypto::HashValue;
use starcoin_genesis::Genesis as StarcoinGenesis;
use starcoin_service_registry::{bus::BusService, RegistryAsyncService, RegistryService};
use starcoin_storage::Store;
use starcoin_sync::block_connector::WriteBlockChainService;
use starcoin_txpool_mock_service::MockTxPoolService;
use starcoin_types::{block::Block, blockhash::ORIGIN, header::Header, startup_info::StartupInfo};

pub async fn create_writeable_block_chain() -> (
    WriteBlockChainService<MockTxPoolService>,
    Arc<NodeConfig>,
    Arc<dyn Store>,
) {
    let node_config = NodeConfig::random_for_test();
    let node_config = Arc::new(node_config);

    let (storage, chain_info, genesis) = StarcoinGenesis::init_storage_for_test(node_config.net())
        .expect("init storage by genesis fail.");
    let registry = RegistryService::launch();
    let bus = registry.service_ref::<BusService>().await.unwrap();
    let txpool_service = MockTxPoolService::new();

    genesis.save(node_config.data_dir()).unwrap();

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

pub fn gen_blocks(
    times: u64,
    writeable_block_chain_service: &mut WriteBlockChainService<MockTxPoolService>,
    time_service: &dyn TimeService,
) {
    let miner_account = AccountInfo::random();
    if times > 0 {
        for _i in 0..times {
            let block = new_block(
                Some(&miner_account),
                writeable_block_chain_service,
                time_service,
            );
            writeable_block_chain_service
                .try_connect(block, None)
                .unwrap();
        }
    }
}

pub fn new_block(
    miner_account: Option<&AccountInfo>,
    writeable_block_chain_service: &mut WriteBlockChainService<MockTxPoolService>,
    time_service: &dyn TimeService,
) -> Block {
    let miner = match miner_account {
        Some(m) => m.clone(),
        None => AccountInfo::random(),
    };
    let miner_address = *miner.address();
    let block_chain = writeable_block_chain_service.get_main();
    let (block_template, _) = block_chain
        .create_block_template(miner_address, None, Vec::new(), vec![], None)
        .unwrap();
    block_chain
        .consensus()
        .create_single_chain_block(block_template, time_service)
        .unwrap()
}

fn main() {
    let _ = async_std::task::block_on(async move {
        let system = actix::prelude::System::new();
        let times = 10;
        let (mut writeable_block_chain_service, node_config, _) =
            create_writeable_block_chain().await;
        let net = node_config.net();
        gen_blocks(
            times,
            &mut writeable_block_chain_service,
            net.time_service().as_ref(),
        );
        assert_eq!(
            writeable_block_chain_service
                .get_main()
                .current_header()
                .number(),
            times
        );
        println!("finished writing blocks");
        system.run().unwrap();
    });

    println!("finish");
}
