//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.

use node_template_runtime::{self, opaque::Block, RuntimeApi};
use sc_client_api::BlockBackend;
use sc_consensus_aura::{ImportQueueParams, SlotProportion, StartAuraParams};
pub use sc_executor::NativeElseWasmExecutor;
use sc_finality_grandpa::SharedVoterState;
use sc_keystore::LocalKeystore;
use sc_service::{error::Error as ServiceError, Configuration, TaskManager};
use sc_telemetry::{Telemetry, TelemetryWorker};
use sp_consensus_aura::sr25519::AuthorityPair as AuraPair;
use std::{sync::Arc, time::Duration};

// Our native executor instance.
pub struct ExecutorDispatch;

impl sc_executor::NativeExecutionDispatch for ExecutorDispatch {
	/// Only enable the benchmarking host functions when we actually want to benchmark.
	#[cfg(feature = "runtime-benchmarks")]
	type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;
	/// Otherwise we only use the default Substrate host functions.
	#[cfg(not(feature = "runtime-benchmarks"))]
	type ExtendHostFunctions = ();

	fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
		node_template_runtime::api::dispatch(method, data)
	}

	fn native_version() -> sc_executor::NativeVersion {
		node_template_runtime::native_version()
	}
}

pub(crate) type FullClient =
	sc_service::TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<ExecutorDispatch>>;
type FullBackend = sc_service::TFullBackend<Block>;
type FullSelectChain = sc_consensus::LongestChain<FullBackend, Block>;

pub fn new_partial(
	config: &Configuration,
) -> Result<
	sc_service::PartialComponents<
		FullClient,
		FullBackend,
		FullSelectChain,
		sc_consensus::DefaultImportQueue<Block, FullClient>,
		sc_transaction_pool::FullPool<Block, FullClient>,
		(
			sc_finality_grandpa::GrandpaBlockImport<
				FullBackend,
				Block,
				FullClient,
				FullSelectChain,
			>,
			sc_finality_grandpa::LinkHalf<Block, FullClient, FullSelectChain>,
			Option<Telemetry>,
		),
	>,
	ServiceError,
> {
	if config.keystore_remote.is_some() {
		return Err(ServiceError::Other("Remote Keystores are not supported.".into()))
	}

	// telemetry：用于向远端节点报告节点的运行状态
	let telemetry = config
		.telemetry_endpoints
		.clone()	// clone()：复制一份配置
		.filter(|x| !x.is_empty())	// filter()：过滤掉空的配置
		.map(|endpoints| -> Result<_, sc_telemetry::Error> {
			let worker = TelemetryWorker::new(16)?;
			let telemetry = worker.handle().new_telemetry(endpoints);
			Ok((worker, telemetry))
		})
		.transpose()?;	// transpose()：将 Result<Option<T>, E> 转换为 Option<Result<T, E>>

	// executor：用于执行 Wasm 代码
	let executor = NativeElseWasmExecutor::<ExecutorDispatch>::new(
		config.wasm_method,	// wasm_method：Wasm 执行方法
		config.default_heap_pages,
		config.max_runtime_instances,
		config.runtime_cache_size,
	);

	// 创建一个完整的 Substrate 客户端：
	// client：客户端，即Substrate架构图中的Client
	// backend：区块链数据存储后端，提供数据存储和访问
	// keystore_container：密钥存储容器
	// task_manager：任务管理器，用于管理异步任务，包括重启、停止等
	let (client, backend, keystore_container, task_manager) =
		sc_service::new_full_parts::<Block, RuntimeApi, _>(
			config,
			telemetry.as_ref().map(|(_, telemetry)| telemetry.handle()),
			executor,
		)?;
	let client = Arc::new(client);

	let telemetry = telemetry.map(|(worker, telemetry)| {
		task_manager.spawn_handle().spawn("telemetry", None, worker.run());
		telemetry
	});

	// select_chain：选择链，即选择最长链。即当有新的区块产生时，
	// LongestChain会比较这个新区块所在的链和当前的最长链，
	// 然后选择其中更长的那个作为新的最长链。
	let select_chain = sc_consensus::LongestChain::new(backend.clone());

	// transaction_pool：交易池，用于存储交易，当有新的交易产生时，会先存储到交易池中；
	// 当验证者准备创建一个新的区块时，会从交易池中选择一些交易放到新的区块中。
	let transaction_pool = sc_transaction_pool::BasicPool::new_full(
		config.transaction_pool.clone(),	// transaction_pool：交易池配置，包括交易的最大数量，交易的最大字节数等
		config.role.is_authority().into(),// 判断当前节点是否是权威节点，如果是，则要验证并处理所有交易，否则只需要转发交易即可
		config.prometheus_registry(),	// prometheus_registry：Prometheus监控注册表，收集和导出交易的数据等。
		task_manager.spawn_essential_handle(),	// 用于创建和管理异步任务
		client.clone(),	// 客户端，用于访问和操作区块链数据，比如验证交易，获取交易的状态等。
	);

	// grandpa_block_import：区块导入器，用于将新的区块导入到本地区块链数据库中。
	let (grandpa_block_import, grandpa_link) = sc_finality_grandpa::block_import(
		client.clone(),
		&(client.clone() as Arc<_>),
		select_chain.clone(),
		telemetry.as_ref().map(|x| x.handle()),
	)?;

	let slot_duration = sc_consensus_aura::slot_duration(&*client)?;

	// import_queue：用于处理和验证新接收到的区块，然后将这些区块导入到本地的区块链数据库中。
	let import_queue =
		// sc_consensus_aura::import_queue()：创建import_queue。
		sc_consensus_aura::import_queue::<AuraPair, _, _, _, _, _>(ImportQueueParams {
			block_import: grandpa_block_import.clone(),// 区块导入器，这里用一个实现了Grandpa共识算法的区块导入器
			justification_import: Some(Box::new(grandpa_block_import.clone())),// 证明导入器，用于处理和验证新区快
			client: client.clone(),	// 区块链客户端，用于访问和操作区块链数据
			create_inherent_data_providers: move |_, ()| async move { // 一个闭包，用于创建内在数据提供者。内在数据是一种特殊交易，它提供了一些区块链的基本信息，比如当前事件，当前槽号等。
				let timestamp = sp_timestamp::InherentDataProvider::from_system_time();

				let slot =
					sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_slot_duration(
						*timestamp,
						slot_duration,
					);

				Ok((slot, timestamp))
			},
			spawner: &task_manager.spawn_essential_handle(),	// 任务管理器，用于创建和管理与import_queue相关的异步任务。
			registry: config.prometheus_registry(),	// 普罗米修斯注册表，用于收集和导出关于import_queue的度量数据，比如队列大小，处理的区块数量等
			check_for_equivocation: Default::default(),	// 是否检查双花攻击。如果为true，则会检查双花攻击，否则不检查。默认为false。
			telemetry: telemetry.as_ref().map(|x| x.handle()), // 用于向远端节点报告import_queue的运行状态
			compatibility_mode: Default::default(),	// 是否启用兼容模式。如果为true，则启用兼容模式，否则不启用。默认为false。
		})?;

	Ok(sc_service::PartialComponents {
		client,
		backend,
		task_manager,
		import_queue,
		keystore_container,
		select_chain,
		transaction_pool,
		other: (grandpa_block_import, grandpa_link, telemetry),
	})
}

fn remote_keystore(_url: &String) -> Result<Arc<LocalKeystore>, &'static str> {
	// FIXME: here would the concrete keystore be built,
	//        must return a concrete type (NOT `LocalKeystore`) that
	//        implements `CryptoStore` and `SyncCryptoStore`
	Err("Remote Keystore not supported.")
}

/// Builds a new service for a full client.把服务启动起来。
pub fn new_full(mut config: Configuration) -> Result<TaskManager, ServiceError> {
	let sc_service::PartialComponents {
		client,
		backend,
		mut task_manager,
		import_queue,
		mut keystore_container,
		select_chain,
		transaction_pool,
		other: (block_import, grandpa_link, mut telemetry),
	} = new_partial(&config)?;

	// 如果keystore_remote 是一个Some(..)值，则执行 if 的代码块，即大括号内的代码。
	// 对在 if let 中拿到的url执行remote_keystore(url)，
	// 如果返回Ok(k)，则执行keystore_container.set_remote_keystore(k)；
	// 如果返回Err(e)，则返回一个ServiceError::Other(e)。
	// 这段代码的作用就是：
	// 如果keystore_remote有值，则执行remote_keystore(url)，并绑定到keystore_container中。
	if let Some(url) = &config.keystore_remote {
		match remote_keystore(url) {
			Ok(k) => keystore_container.set_remote_keystore(k),
			Err(e) =>
				return Err(ServiceError::Other(format!(
					"Error hooking up remote keystore for {}: {}",
					url, e
				))),
		};
	}
	// 生成Grandpa 共识协议的名称。在Substrate中，每个共识协议都有一个唯一的名称，
	// 用于在网络中识别和区分不同的共识协议，以便其他节点知道如何与当前节点进行通信。
	// `sc_finality_grandpa::protocol_standard_name`函数用于生成Grandpa共识协议的名称。这个函数接受两个参数：
	// 1. `&client.block_hash(0).ok().flatten().expect("Genesis block exists; qed")`：
	// 这个参数是创世区块的哈希值。
	// `client.block_hash(0)`用于获取创世区块的哈希值，参数表示区块高度，高度为0即创世区块。
	// 2. `&config.chain_spec`：这个参数是链的规范。链的规范是一个包含了链的各种配置和参数的对象，
	// 比如链的名称、链的版本、链的创世区块等。
	let grandpa_protocol_name = sc_finality_grandpa::protocol_standard_name(
		&client.block_hash(0).ok().flatten().expect("Genesis block exists; qed"),
		&config.chain_spec,
	);

	config
		.network
		.extra_sets
		.push(sc_finality_grandpa::grandpa_peers_set_config(grandpa_protocol_name.clone()));
	let warp_sync = Arc::new(sc_finality_grandpa::warp_proof::NetworkProvider::new(
		backend.clone(),
		grandpa_link.shared_authority_set().clone(),
		Vec::default(),
	));

	// 配置网络
	let (network, system_rpc_tx, tx_handler_controller, network_starter) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue,
			block_announce_validator_builder: None,
			warp_sync: Some(warp_sync),
		})?;

	// 配置Offchain Worker，即调用`sc_service::build_offchain_workers`函数，
	// 在Substrate中，有一种叫做Offchain Worker的机制，用于在链外执行一些任务，
	// 比如获取外部数据，执行一些计算任务等。这个机制可以用于实现一些链外的功能，
	if config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&config,
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	let role = config.role.clone();
	let force_authoring = config.force_authoring;
	let backoff_authoring_blocks: Option<()> = None;
	let name = config.network.node_name.clone();
	let enable_grandpa = !config.disable_grandpa;
	let prometheus_registry = config.prometheus_registry().cloned();

	// 创建RPC扩展构建器
	let rpc_extensions_builder = {
		let client = client.clone();
		let pool = transaction_pool.clone();

		Box::new(move |deny_unsafe, _| {
			let deps =
				crate::rpc::FullDeps { client: client.clone(), pool: pool.clone(), deny_unsafe };
			crate::rpc::create_full(deps).map_err(Into::into)
		})
	};

	// 启动并配置任务管理器
	// `sc_service::spawn_tasks(...)`函数的主要作用是启动一系列的后台任务，
	// 这些任务用于处理区块链的各种操作，如网络通信、交易处理、RPC处理等。
	// 这些任务通常会在新的线程或异步任务中运行，以便并行处理。
	// 同时，`sc_service::spawn_tasks(...)`函数也会返回一个`RpcHandlers`对象，
	// 这个对象提供了一些方法，可以用来操作和管理RPC服务，如启动RPC服务、停止RPC服务等。
	// 以下代码中，这个`RpcHandlers`对象被绑定到了`_rpc_handlers`变量。
	let _rpc_handlers = sc_service::spawn_tasks(sc_service::SpawnTasksParams {
		network: network.clone(),
		client: client.clone(),
		keystore: keystore_container.sync_keystore(),
		task_manager: &mut task_manager,
		transaction_pool: transaction_pool.clone(),
		rpc_builder: rpc_extensions_builder,
		backend,
		system_rpc_tx,
		tx_handler_controller,
		config,
		telemetry: telemetry.as_mut(),
	})?;

	// 如果当前节点是权威节点，则启动AURA共识算法服务
	if role.is_authority() {
		let proposer_factory = sc_basic_authorship::ProposerFactory::new(
			task_manager.spawn_handle(),
			client.clone(),
			transaction_pool,
			prometheus_registry.as_ref(),
			telemetry.as_ref().map(|x| x.handle()),
		);

		let slot_duration = sc_consensus_aura::slot_duration(&*client)?;

		// 启动Aura共识算法服务。Aura是一种基于时间槽的共识协议，每个时间槽都有一个预定的验证者负责产生新的区块。
		// start_aura()返回一个Future，需要在FutureExecutor中启动，即通过后续代码在task_manager中启动
		let aura = sc_consensus_aura::start_aura::<AuraPair, _, _, _, _, _, _, _, _, _, _>(	
			StartAuraParams {
				slot_duration,	// 时间槽的持续时间。在每个时间槽内，只有一个验证者有权产生新的区块
				client,	// 区块链客户端，用于访问和操作区块链数据。
				select_chain,	// 选择链策略，这里是用最长链策略
				block_import,	// 区块导入器，用于将新的区块导入到本地的区块链数据库中。
				proposer_factory,	// 提议者工厂，用于创建新的提议者。提议者是负责产生新的区块的实体，类似波卡中的收集器，收集待处理的交易，打包成新区快给验证者，验证通过，验证者会将新的区块添加到区块链中。
				create_inherent_data_providers: move |_, ()| async move {	// 闭包，创建内在数据提供者的函数。内在数据是一种特殊的交易，它提供了一些区块链的基本信息，比如当前的时间、当前的槽号等。
					let timestamp = sp_timestamp::InherentDataProvider::from_system_time();

					let slot =
						sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_slot_duration(
							*timestamp,
							slot_duration,
						);

					Ok((slot, timestamp))
				},
				force_authoring,	// 一个布尔值，表示是否强制产生新的区块，即使没有新的交易。
				backoff_authoring_blocks,	// 一个可选的区块哈希集合，表示需要回退的区块。
				keystore: keystore_container.sync_keystore(),	// 密钥库，用于存储和管理验证者的密钥。
				sync_oracle: network.clone(),	// 同步预言机，用于判断区块链是否已经同步。
				justification_sync_link: network.clone(),	// 证明同步链接，用于同步区块的证明。
				block_proposal_slot_portion: SlotProportion::new(2f32 / 3f32),	// 区块提议槽的比例。这个比例表示一个验证者在一个时间槽内可以用于产生新的区块的时间比例。
				max_block_proposal_slot_portion: None,	// 最大的区块提议槽的比例。这个比例表示一个验证者在一个时间槽内最多可以用于产生新的区块的时间比例。
				telemetry: telemetry.as_ref().map(|x| x.handle()), // 可选的遥测句柄，用于发送关于Aura共识协议的各种遥测数据。
				compatibility_mode: Default::default(), // 一个布尔值，表示是否启用兼容模式。如果为`true`，那么Aura共识协议会使用一些旧的、可能不那么高效的算法和协议，以确保与旧版本的节点兼容。
			},
		)?;

		// the AURA authoring task is considered essential, i.e. if it
		// fails we take down the service with it.
		task_manager
			.spawn_essential_handle()	// 获取关键任务 SpawnEssentialTaskHandle对象
			// spawn_blocking()：通过SpawnEssentialTaskHandle对象启动一个名为“aura”的任务，
			// 第二个参数Some("block-authoring")，是任务的类型，表示这个任务是用来产生新的区块的。
			// 第三个参数aura，是一个闭包，定义了任务的具体行为。
			.spawn_blocking("aura", Some("block-authoring"), aura);
	}

	// 如果
	if enable_grandpa {
		// if the node isn't actively participating in consensus then it doesn't
		// need a keystore, regardless of which protocol we use below.
		let keystore =
			if role.is_authority() { Some(keystore_container.sync_keystore()) } else { None };

		let grandpa_config = sc_finality_grandpa::Config {
			// FIXME #1578 make this available through chainspec
			gossip_duration: Duration::from_millis(333),
			justification_period: 512,
			name: Some(name),
			observer_enabled: false,
			keystore,
			local_role: role,
			telemetry: telemetry.as_ref().map(|x| x.handle()),
			protocol_name: grandpa_protocol_name,
		};

		// start the full GRANDPA voter
		// NOTE: non-authorities could run the GRANDPA observer protocol, but at
		// this point the full voter should provide better guarantees of block
		// and vote data availability than the observer. The observer has not
		// been tested extensively yet and having most nodes in a network run it
		// could lead to finality stalls.
		let grandpa_config = sc_finality_grandpa::GrandpaParams {
			config: grandpa_config,
			link: grandpa_link,
			network,
			voting_rule: sc_finality_grandpa::VotingRulesBuilder::default().build(),
			prometheus_registry,
			shared_voter_state: SharedVoterState::empty(),
			telemetry: telemetry.as_ref().map(|x| x.handle()),
		};

		// the GRANDPA voter task is considered infallible, i.e.
		// if it fails we take down the service with it.
		task_manager.spawn_essential_handle().spawn_blocking(
			"grandpa-voter",
			None,
			sc_finality_grandpa::run_grandpa_voter(grandpa_config)?,
		);
	}

	network_starter.start_network();
	Ok(task_manager)
}
