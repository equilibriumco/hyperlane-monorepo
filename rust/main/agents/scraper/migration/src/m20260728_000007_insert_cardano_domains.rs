use sea_orm::prelude::TimeDateTime;
use time::OffsetDateTime;

use sea_orm_migration::prelude::*;

use crate::m20230309_000001_create_table_domain::Domain;

/// Cardano domains the scraper can index.
///
/// These live in their own migration rather than in the original `DOMAINS`
/// seed, which must stay immutable so existing databases don't need a full
/// rollback to pick up new chains.
///
/// Cardano has no EVM chain ID; `chain_id` mirrors the Hyperlane domain, which
/// is what the agent configs already use.
const CARDANO_DOMAINS: &[RawDomain] = &[
    RawDomain {
        name: "cardano",
        domain: 2001,
        is_test_net: false,
    },
    RawDomain {
        name: "cardanopreprod",
        domain: 2002,
        is_test_net: true,
    },
    RawDomain {
        name: "cardanopreview",
        domain: 2003,
        is_test_net: true,
    },
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm_migration::sea_orm::ActiveValue::Set;
        use sea_orm_migration::sea_orm::EntityTrait;

        let db = manager.get_connection();
        for domain in CARDANO_DOMAINS {
            let now = {
                let offset = OffsetDateTime::now_utc();
                TimeDateTime::new(offset.date(), offset.time())
            };

            EntityTrait::insert(domain::ActiveModel {
                id: Set(domain.domain),
                time_created: Set(now),
                time_updated: Set(now),
                name: Set(domain.name.to_owned()),
                native_token: Set("ADA".to_owned()),
                chain_id: Set(domain.domain.into()),
                is_test_net: Set(domain.is_test_net),
                is_deprecated: Set(false),
            })
            .exec(db)
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .exec_stmt(
                Query::delete()
                    .from_table(Domain::Table)
                    .and_where(
                        Expr::col(Domain::Id)
                            .is_in(CARDANO_DOMAINS.iter().map(|d| d.domain).collect::<Vec<_>>()),
                    )
                    .to_owned(),
            )
            .await
    }
}

mod domain {
    use sea_orm_migration::sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "domain")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        id: u32,
        time_created: TimeDateTime,
        time_updated: TimeDateTime,
        name: String,
        native_token: String,
        chain_id: u64,
        is_test_net: bool,
        is_deprecated: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

struct RawDomain {
    name: &'static str,
    domain: u32,
    is_test_net: bool,
}
