use diesel::prelude::*;
use diesel::pg::{Pg, PgConnection};
use pg::GenericConnection;
use pg::rows::Row;
use semver;

use Model;
use git;
use krate::{Crate, canon_crate_name};
use schema::*;
use util::{CargoResult, human};

pub struct Dependency {
    pub id: i32,
    pub version_id: i32,
    pub crate_id: i32,
    pub req: semver::VersionReq,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
    pub target: Option<String>,
    pub kind: Kind,
}

pub struct ReverseDependency {
    dependency: Dependency,
    crate_name: String,
    crate_downloads: i32,
}

#[derive(RustcEncodable, RustcDecodable)]
pub struct EncodableDependency {
    pub id: i32,
    pub version_id: i32,
    pub crate_id: String,
    pub req: String,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
    pub target: Option<String>,
    pub kind: Kind,
    pub downloads: i32,
}

#[derive(Copy, Clone)]
#[repr(u32)]
pub enum Kind {
    Normal = 0,
    Build = 1,
    Dev = 2,
    // if you add a kind here, be sure to update `from_row` below.
}

#[derive(Insertable)]
#[table_name="dependencies"]
struct NewDependency<'a> {
    version_id: i32,
    crate_id: i32,
    req: String,
    optional: bool,
    default_features: bool,
    features: Vec<&'a str>,
    target: Option<&'a str>,
    kind: i32,
}

impl Dependency {
    // FIXME: Encapsulate this in a `NewDependency` struct
    #[cfg_attr(feature = "clippy", allow(too_many_arguments))]
    pub fn insert(conn: &GenericConnection, version_id: i32, crate_id: i32,
                  req: &semver::VersionReq, kind: Kind,
                  optional: bool, default_features: bool,
                  features: &[String], target: &Option<String>)
                  -> CargoResult<Dependency> {
        let req = req.to_string();
        let stmt = conn.prepare("INSERT INTO dependencies
                                      (version_id, crate_id, req, optional,
                                       default_features, features, target, kind)
                                      VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                      RETURNING *")?;
        let rows = stmt.query(&[&version_id, &crate_id, &req,
            &optional, &default_features,
            &features, target, &(kind as i32)])?;
        Ok(Model::from_row(&rows.iter().next().unwrap()))
    }

    pub fn git_encode(self, crate_name: &str) -> git::Dependency {
        git::Dependency {
            name: crate_name.into(),
            req: self.req.to_string(),
            features: self.features,
            optional: self.optional,
            default_features: self.default_features,
            target: self.target,
            kind: Some(self.kind),
        }
    }

    // `downloads` need only be specified when generating a reverse dependency
    pub fn encodable(self, crate_name: &str, downloads: Option<i32>) -> EncodableDependency {
        EncodableDependency {
            id: self.id,
            version_id: self.version_id,
            crate_id: crate_name.into(),
            req: self.req.to_string(),
            optional: self.optional,
            default_features: self.default_features,
            features: self.features,
            target: self.target,
            kind: self.kind,
            downloads: downloads.unwrap_or(0),
        }
    }
}

impl ReverseDependency {
    pub fn encodable(self) -> EncodableDependency {
        self.dependency.encodable(&self.crate_name, Some(self.crate_downloads))
    }
}

pub fn add_dependencies(
    conn: &PgConnection,
    deps: &[::upload::CrateDependency],
    version_id: i32,
) -> CargoResult<Vec<Dependency>> {
    use diesel::insert;
    use diesel::expression::dsl::any;

    let crate_names = deps.iter().map(|d| &*d.name).collect::<Vec<_>>();
    let crates = Crate::all()
        .filter(canon_crate_name(crates::name).eq(any(crate_names)))
        .load::<Crate>(conn)?;

    let new_dependencies = deps.iter().map(|dep| {
        let krate = crates.iter().find(|c| dep.name == c.name)
            .map(Ok)
            .unwrap_or_else(|| {
                Err(human(&format_args!("no known crate named `{}`", &*dep.name)))
            })?;
        if dep.version_req == semver::VersionReq::parse("*").unwrap() {
            return Err(human("wildcard (`*`) dependency constraints are not allowed \
                              on crates.io. See http://doc.crates.io/faq.html#can-\
                              libraries-use--as-a-version-for-their-dependencies for more \
                              information"));
        }
        let features = dep.features.iter().map(|s| &**s).collect();
        Ok(NewDependency {
            version_id: version_id,
            crate_id: krate.id,
            req: dep.version_req.to_string(),
            kind: dep.kind.unwrap_or(Kind::Normal) as i32,
            optional: dep.optional,
            default_features: dep.default_features,
            features: features,
            target: dep.target.as_ref().map(|s| &**s),
        })
    }).collect::<Result<Vec<_>, _>>()?;

    insert(&new_dependencies).into(dependencies::table)
        .get_results(conn)
        .map_err(Into::into)
}

impl Queryable<dependencies::SqlType, Pg> for Dependency {
    type Row = (i32, i32, i32, String, bool, bool, Vec<String>, Option<String>,
                i32);

    fn build(row: Self::Row) -> Self {
        Dependency {
            id: row.0,
            version_id: row.1,
            crate_id: row.2,
            req: semver::VersionReq::parse(&row.3).unwrap(),
            optional: row.4,
            default_features: row.5,
            features: row.6,
            target: row.7,
            kind: match row.8 {
                0 => Kind::Normal,
                1 => Kind::Build,
                2 => Kind::Dev,
                n => panic!("unknown kind: {}", n),
            }
        }
    }
}

impl Model for Dependency {
    fn from_row(row: &Row) -> Dependency {
        let req: String = row.get("req");
        Dependency {
            id: row.get("id"),
            version_id: row.get("version_id"),
            crate_id: row.get("crate_id"),
            req: semver::VersionReq::parse(&req).unwrap(),
            optional: row.get("optional"),
            default_features: row.get("default_features"),
            features: row.get("features"),
            target: row.get("target"),
            kind: match row.get("kind") {
                0 => Kind::Normal,
                1 => Kind::Build,
                2 => Kind::Dev,
                n => panic!("unknown kind: {}", n),
            }
        }
    }

    fn table_name(_: Option<Dependency>) -> &'static str { "dependencies" }
}

impl Model for ReverseDependency {
    fn from_row(row: &Row) -> Self {
        ReverseDependency {
            dependency: Model::from_row(row),
            crate_name: row.get("crate_name"),
            crate_downloads: row.get("crate_downloads"),
        }
    }

    fn table_name(_: Option<Self>) -> &'static str { panic!("no table") }
}
