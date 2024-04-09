// 如果未启用 `std`进行编译，那么将启用`no_std`
#![cfg_attr(not(feature = "std"), no_std)]

// 将模块 pallet 的所有item 导出
pub use pallet::*;

/// define a module: pallet
#[frame_support::pallet]
pub mod pallet {
    // frame_support::pallet_prelude：包含常用的存储解构StorageValue, StorageMap, StorageDoubleMap等，
    // 宏定义，如pallet::pallet, pallet::event, pallet::error等
    // 常用接口trait：ConstU32, EnsureOrigin, Get, GetDefault, GetStorageVersion, Hooks, IsType, PalletInfoAccess, StorageInfoTrait, StorageVersion, TypedGet,
    // 常用Hash算法：Blake2_128, Blake2_256, Keccak256, Sha3_256, Twox128, Twox64
    use frame_support::{dispatch::DispatchResult, pallet_prelude::*};
    // frame_system 是Substrate框架的核心库，提供了区块链系统的基本功能，
    // 如区块和交易的处理，账户管理等。
    // frame_system::pallet_prelude：包含了在构建pallet时常用的类型和宏定义，包括：
    // BlockNumberFor、OriginFor、ensure_none, ensure_root, ensure_signed, ensure_signed_or_root等
    use frame_system::pallet_prelude::*;

    #[pallet::config]
    // 定义了一个名为Config的trait，用于定义pallet的配置参数
    // 该trait继承了frame_system::Config，也就拥有了frame_system::Config里面的数据类型，包括：
    // BlockNumber, Hash, AccountId等
    pub trait Config: frame_system::Config {
        // 第一个配置项：声明一个关联类型，用于存储存证的最大长度
        /// The maximum length of a claim that can be added to the blockchain
        #[pallet::constant] // 定义常量
        type MaxClaimLength: Get<u32>;
        // 第二个配置项：声明一个关联类型，表示存证的事件类型
        /// Because this pallet emits events, it depends on the runtime's definition of an event.
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
    }

    // pallet::pallet宏:指明这是一个pallet
    #[pallet::pallet]
    // generate_store宏：用于生成存储的相关代码，即
    // 生成一个名为`Store`的trait，以及这个trait所有存储项的getter方法。
    // pub(super)：是一个可见性修饰符，表示这个`Store` trait只在当前模块及其父模块中可见
    #[pallet::generate_store(pub(super) trait Store)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn proofs)]
    pub type Proofs<T: Config> = StorageMap<
        _,  // 前缀类型，在Substrate中，每个存储项都有一个前缀，用于在底层存储中区分不同的存储项。这里表示使用默认的前缀，这个默认的前缀是由pallet的名称和存储项的名称组成的。
        Blake2_128Concat,   // 这是StorageMap的哈希器类型。在Substrate中，存储项的键会被哈希化，以减少存储空间的使用和提高查找效率。Blake2_128Concat是一种哈希算法，它会生成一个128位的哈希值，并将原始键值连接到哈希值的后面。
        // BoundedVec有界向量保存的是存证的hash值，这个hash值是存证的内容的hash值
        BoundedVec<u8, T::MaxClaimLength>,  // 这是StorageMap的键类型。在这个例子中，键是一个有界的字节向量，该向量的u8元素数量不超过T::MaxClaimLength。也意味着不能使用长度超过MaxClaimLength的字节序列作为这个StorageMap的键。
        (T::AccountId, T::BlockNumber), // 这是StorageMap的值类型。在这个例子中，值是一个元组，包含一个账户ID和一个区块号
        >;

    #[pallet::event]
    // 生成一个名为 deposit_event 的函数，用于触发事件
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        // 存证成功创建
        ClaimCreated(T::AccountId, BoundedVec<u8, T::MaxClaimLength>),
        // 存证成功撤销
        ClaimnRevoked(T::AccountId, BoundedVec<u8, T::MaxClaimLength>),
    }

    #[pallet::error]
    pub enum Error<T> {
        ProofAlreadyExist,
        ClaimTooLong,
        ClaimNotExist,
        NotClaimOwner,
    }

    // #[pallet::hooks]宏：用于定义pallet的钩子函数
    // 这些钩子函数通常包括： on_initialize，on_finalize，on_runtime_upgrade等等
    #[pallet::hooks]
    // 为上面定义的Pallet结构体实现钩子函数：Hooks<BlockNumberFor<T>>
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}   // 没有定义钩子函数

    // 为上面定义的Pallet添加可公开调用函数，即对外的接口
    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]    // 指定调用索引
        #[pallet::weight(0)]    // 指定权重
        pub fn create_claim(
            origin: OriginFor<T>,   // 交易的发送者，即调用的源
            claim: BoundedVec<u8, T::MaxClaimLength>    // 要创建的存证的内容
        ) -> DispatchResult {
            // ensure_signed函数：检查交易的发送者是否是签名的账户
            // 如果是，则返回签名的账户ID，否则返回错误
            let sender = ensure_signed(origin)?;
            // ensure!宏:用于检查条件是否为真，如果为真，则返回Ok(())，否则返回Err(错误信息)
            // 在这里检查存证内容是否已存在，如果存在则返回错误
            ensure!(!Proofs::<T>::contains_key(&claim), Error::<T>::ProofAlreadyExist);

            // 如果以上校验通过，则将存证内容和存证的拥有者信息存入Proofs映射中
            Proofs::<T>::insert(
                &claim,
                (sender.clone(), frame_system::Pallet::<T>::block_number())
            );

            // 触发存证创建事件
            Self::deposit_event(Event::ClaimCreated(sender, claim));
            // Self关键字代表当前的Pallet<T>实例
            // deposit_event函数用于触发事件，它会将事件数据写入到区块链中
            // 该函数是在#[pallet::generate_deposit(pub(super) fn deposit_event)]宏中生成的。

            Ok(().into())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(0)]
        pub fn revoke_claim(
            origin: OriginFor<T>,
            claim: BoundedVec<u8, T::MaxClaimLength>,
        ) -> DispatchResult {
            // 检验请求发送方是否是签名用户
            let sender = ensure_signed(origin)?;
            // 检查存证是否存在
            ensure!(Proofs::<T>::contains_key(&claim), Error::<T>::ClaimNotExist);

            let (owner, _) = Proofs::<T>::get(&claim).ok_or(Error::<T>::NotClaimOwner)?;
            // let (owner, _) = Self::proofs(&claim).ok_or(Error::<T>::NotClaimOwner)?; // 这里的ok_or()不支持传入错误类型
            // 检查请求发送方是否是存证的拥有者
            ensure!(owner == sender, Error::<T>::NotClaimOwner);

            // 从Proofs映射中移除存证
            Proofs::<T>::remove(&claim);

            // 触发存证撤销事件
            Self::deposit_event(Event::ClaimnRevoked(sender, claim));

            Ok(().into())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(0)]
        pub fn transfer_claim(
            origin: OriginFor<T>,
            claim: BoundedVec<u8, T::MaxClaimLength>,
            to_account_id: T::AccountId,
        ) -> DispatchResult {
            // 检验请求发送方是否是签名用户
            let sender = ensure_signed(origin)?;
            // 检查存证是否存在
            ensure!(Proofs::<T>::contains_key(&claim), Error::<T>::ClaimNotExist);

            let (owner, _) = Proofs::<T>::get(&claim).ok_or(Error::<T>::NotClaimOwner)?;
            // 检查请求发送方是否是存证的拥有者
            ensure!(owner == sender, Error::<T>::NotClaimOwner);

            // 更新存证的拥有者
            Proofs::<T>::insert(
                &claim,
                (to_account_id.clone(), frame_system::Pallet::<T>::block_number())
            );

            Ok(().into())
        }
    }
    
}