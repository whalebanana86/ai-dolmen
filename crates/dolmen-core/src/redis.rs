//! Redis sessions.
//!
//! Each [`Session`] holds one multiplexed connection, which the editor and the
//! key browser share. Commands that would change the connection's state under
//! everything else using it (`SELECT`, `MULTI`) or take it over (`SUBSCRIBE`,
//! `MONITOR`) are refused; the database changes through [`Session::select`].

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ::redis::aio::MultiplexedConnection;
use ::redis::{
    AsyncConnectionConfig, Cmd, ErrorKind, ProtocolVersion, RedisConnectionInfo, RedisError, Value,
};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::postgres::{ConnectConfig, SslMode, StatementResult};
use crate::{Error, Refusal};

mod command;
mod reply;

use command::parse;
pub use command::quote;
use reply::{bytes_text, statement_result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How many keys SCAN looks at per call. A hint to the server, not a limit.
const SCAN_COUNT: usize = 500;
/// How many TYPE commands go in one pipeline, so a big listing isn't one huge request.
const TYPE_BATCH: usize = 1000;
/// Redis's default, for servers that won't say, as managed services refusing CONFIG.
const DEFAULT_DATABASES: u32 = 16;

/// Keys found by [`Session::keys`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Keys {
    pub keys: Vec<Key>,
    /// False when the limit cut the scan short, so there may be more keys.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Key {
    /// The key's name. A name that isn't UTF-8 is `\x` and its bytes in hex,
    /// as values are shown; [`quote`] turns it back into the same bytes.
    pub name: String,
    /// `string`, `list`, `set`, `zset`, `hash`, `stream`, `ReJSON-RL`, ... as TYPE says.
    pub kind: String,
}

/// The server's databases, as [`Session::databases`] finds them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Databases {
    /// How many databases the server has, numbered from 0.
    pub count: u32,
    /// (index, keys) for the databases that have keys, by index.
    pub used: Vec<(u32, u64)>,
}

/// A connection to a Redis server, or one that speaks its protocol, such as Valkey.
pub struct Session {
    connection: MultiplexedConnection,
    /// RESP3 unless the server is older than Redis 6; [`another`](Self::another)
    /// uses the same rather than finding out again.
    protocol: ProtocolVersion,
    server_version: String,
    config: ConnectConfig,
    database: AtomicU32,
    /// What COMMAND INFO said about each command, by lowercase name, for
    /// read-only checks. `None` for commands the server doesn't know.
    commands: Mutex<HashMap<String, Option<CommandInfo>>>,
}

/// Whether a command writes, from its COMMAND INFO flags.
struct CommandInfo {
    writes: bool,
    /// Whether each subcommand writes, by lowercase name, for container
    /// commands such as CONFIG on servers that describe them (Redis 7 and later).
    subcommands: HashMap<String, bool>,
}

impl Session {
    /// Connects to the server in `config`.
    ///
    /// `config.database` is the database number; empty means 0. With an empty
    /// `config.user`, the password alone is sent (the `requirepass` way), and
    /// nothing without one; otherwise the user logs in as an ACL user.
    ///
    /// Redis has no TLS negotiation, so `prefer` can't upgrade and connects
    /// in plain text like `disable`; `require` uses TLS without verifying the
    /// certificate, and `verify-full` verifies the chain and host name.
    pub async fn connect(config: &ConnectConfig) -> Result<Session, Error> {
        let database = database_number(&config.database)?;
        // RESP3 first, for its typed replies; Redis before 6 doesn't know HELLO.
        let (connection, protocol) = open(
            config,
            database,
            &[ProtocolVersion::RESP3, ProtocolVersion::RESP2],
        )
        .await?;
        Session::start(config, connection, protocol, database).await
    }

    async fn start(
        config: &ConnectConfig,
        connection: MultiplexedConnection,
        protocol: ProtocolVersion,
        database: u32,
    ) -> Result<Session, Error> {
        let mut session = Session {
            connection,
            protocol,
            server_version: String::new(),
            config: config.clone(),
            database: AtomicU32::new(database),
            commands: Mutex::new(HashMap::new()),
        };
        session.server_version = session.read_server_version().await?;
        Ok(session)
    }

    /// The server's version, from INFO: `valkey_version` on Valkey, which
    /// also reports the Redis version it is compatible with, else `redis_version`.
    /// Empty when the server won't run INFO.
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    async fn read_server_version(&self) -> Result<String, Error> {
        let info = match self.query(::redis::cmd("INFO").arg("server")).await {
            Ok(reply) => text(reply).unwrap_or_default(),
            // The version is only shown, so a server refusing INFO can still be used.
            Err(Error::Database { .. }) => return Ok(String::new()),
            Err(err) => return Err(err),
        };
        let field = |name: &str| {
            info.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .map(|value| value.trim().to_owned())
        };
        Ok(field("valkey_version")
            .or_else(|| field("redis_version"))
            .unwrap_or_default())
    }

    /// The settings the session connected with.
    pub fn config(&self) -> &ConnectConfig {
        &self.config
    }

    /// Another connection to the same server, in the database this one is in now.
    pub async fn another(&self) -> Result<Session, Error> {
        let database = self.database();
        let (connection, protocol) = open(&self.config, database, &[self.protocol]).await?;
        Session::start(&self.config, connection, protocol, database).await
    }

    /// The database the session's commands run in.
    pub fn database(&self) -> u32 {
        self.database.load(Ordering::Relaxed)
    }

    /// Switches the session to another database, for every command that
    /// shares the connection from now on.
    pub async fn select(&self, database: u32) -> Result<(), Error> {
        self.query(::redis::cmd("SELECT").arg(database)).await?;
        self.database.store(database, Ordering::Relaxed);
        Ok(())
    }

    /// Runs commands, one per line, and returns a result per command.
    ///
    /// Lines are split into arguments as redis-cli does: on spaces, with
    /// `"double quotes"` taking `\n`, `\t`, `\"`, `\\` and `\xHH` escapes and
    /// `'single quotes'` taking text as it is. Blank lines and lines starting
    /// with `#` are skipped. Every command is checked before any runs, then
    /// they run in order until one fails. At most `max_rows` rows are kept
    /// per result; the rest are only counted.
    ///
    /// With `read_only`, commands that write are refused: those COMMAND INFO
    /// flags `write`, scripts that could write (EVAL rather than EVAL_RO), and
    /// admin commands that change the server, such as FLUSHALL or CONFIG SET.
    /// A guard against mistakes rather than a permission, as for PostgreSQL.
    pub async fn execute(
        &self,
        text: &str,
        max_rows: usize,
        read_only: bool,
    ) -> Result<Vec<StatementResult>, Error> {
        let commands = parse(text)?;
        for command in &commands {
            refuse_unsupported(&command.args)?;
            if read_only {
                self.refuse_writes(&command.args).await?;
            }
        }

        let mut results = Vec::with_capacity(commands.len());
        for command in &commands {
            let mut cmd = Cmd::new();
            for arg in &command.args {
                cmd.arg(arg.as_slice());
            }
            let reply = self.query(&cmd).await.map_err(|err| match err {
                Error::Database {
                    message,
                    code,
                    detail,
                    hint,
                    ..
                } => Error::Database {
                    message,
                    code,
                    detail,
                    hint,
                    position: Some(command.position),
                },
                other => other,
            })?;
            results.push(statement_result(reply, &command.args, max_rows));
        }
        Ok(results)
    }

    /// Refuses a command that writes or changes the server, for a read-only connection.
    async fn refuse_writes(&self, args: &[Vec<u8>]) -> Result<(), Error> {
        let name = upper(&args[0]);
        let sub = args.get(1).map(|arg| upper(arg)).unwrap_or_default();
        let refuse = |message: String| {
            Err(Error::Refused {
                reason: Refusal::ReadOnly,
                message,
            })
        };
        // COMMAND INFO doesn't flag these as writes, but they change or stop
        // the server, or every database at once.
        let dangerous = match name.as_str() {
            "SHUTDOWN" | "DEBUG" | "FLUSHALL" | "FLUSHDB" | "REPLICAOF" | "SLAVEOF"
            | "FAILOVER" | "MIGRATE" | "SWAPDB" => true,
            "CONFIG" => matches!(sub.as_str(), "SET" | "REWRITE" | "RESETSTAT"),
            "SCRIPT" => sub == "FLUSH",
            "FUNCTION" => matches!(sub.as_str(), "DELETE" | "FLUSH" | "LOAD" | "RESTORE"),
            "CLIENT" => sub == "KILL",
            "ACL" => matches!(sub.as_str(), "SETUSER" | "DELUSER"),
            "MODULE" => matches!(sub.as_str(), "LOAD" | "LOADEX" | "UNLOAD"),
            _ => false,
        };
        if dangerous {
            let command = if is_container(&name) {
                format!("{name} {sub}")
            } else {
                name
            };
            return refuse(format!(
                "{command} isn't allowed while the connection is read-only."
            ));
        }
        // Scripts aren't flagged as writes, but can write; the server makes
        // sure the _RO variants don't.
        if matches!(name.as_str(), "EVAL" | "EVALSHA" | "FCALL") {
            return refuse(format!(
                "{name} can run commands that write, and the connection is read-only. Use {name}_RO instead."
            ));
        }
        if let Some(command) = self.writing_command(args).await? {
            return refuse(format!(
                "{command} writes, and the connection is read-only."
            ));
        }
        Ok(())
    }

    /// The command's name, with its subcommand for containers, when COMMAND
    /// INFO flags it as writing. Answers are kept, so each command is asked once.
    async fn writing_command(&self, args: &[Vec<u8>]) -> Result<Option<String>, Error> {
        let name = String::from_utf8_lossy(&args[0]).to_ascii_lowercase();
        if !self.commands.lock().unwrap().contains_key(&name) {
            let info = match self
                .query(::redis::cmd("COMMAND").arg("INFO").arg(&name))
                .await
            {
                Ok(reply) => command_info(reply),
                // Without an answer there's no telling, and the guard is for safety.
                Err(Error::Database { message, .. }) => {
                    return Err(Error::Refused {
                        reason: Refusal::ReadOnly,
                        message: format!(
                            "Can't tell whether {} writes, because the server didn't answer COMMAND INFO ({message}), and the connection is read-only.",
                            name.to_ascii_uppercase()
                        ),
                    });
                }
                Err(err) => return Err(err),
            };
            self.commands.lock().unwrap().insert(name.clone(), info);
        }

        let commands = self.commands.lock().unwrap();
        // A command the server doesn't know fails there.
        let Some(info) = commands.get(&name).and_then(Option::as_ref) else {
            return Ok(None);
        };
        if let Some(sub) = args.get(1) {
            let sub = String::from_utf8_lossy(sub).to_ascii_lowercase();
            if let Some(&writes) = info.subcommands.get(&sub) {
                return Ok(writes.then(|| {
                    format!("{} {}", name.to_ascii_uppercase(), sub.to_ascii_uppercase())
                }));
            }
        }
        Ok(info.writes.then(|| name.to_ascii_uppercase()))
    }

    /// Keys whose names match `pattern` (`*` when empty), with their types,
    /// sorted by name. Stops once `limit` keys are found: SCAN walks the
    /// whole keyspace, which takes long on a big database.
    pub async fn keys(&self, pattern: &str, limit: usize) -> Result<Keys, Error> {
        let pattern = if pattern.is_empty() { "*" } else { pattern };
        let mut names = Vec::new();
        // SCAN can return a key twice while the keyspace is being resized.
        let mut seen = HashSet::new();
        let mut cursor = b"0".to_vec();
        let mut complete = false;
        while names.len() < limit {
            let reply = self
                .query(
                    ::redis::cmd("SCAN")
                        .arg(cursor.as_slice())
                        .arg("MATCH")
                        .arg(pattern)
                        .arg("COUNT")
                        .arg(SCAN_COUNT),
                )
                .await?;
            let (next, batch) = scan_reply(reply).ok_or_else(|| Error::Connection {
                message: "the server sent an unexpected reply to SCAN".into(),
            })?;
            for name in batch {
                if seen.insert(name.clone()) {
                    names.push(name);
                }
            }
            if next == b"0" {
                complete = true;
                break;
            }
            cursor = next;
        }
        if names.len() > limit {
            names.truncate(limit);
            complete = false;
        }

        let mut keys = Vec::with_capacity(names.len());
        let mut connection = self.connection.clone();
        for batch in names.chunks(TYPE_BATCH) {
            let mut pipeline = ::redis::pipe();
            for name in batch {
                pipeline.cmd("TYPE").arg(name.as_slice());
            }
            let replies = connection
                .send_packed_commands(&pipeline, 0, batch.len())
                .await?;
            for (name, reply) in batch.iter().zip(replies) {
                // A key deleted since SCAN is "none"; one the user's ACL
                // doesn't allow is an error. Neither can be shown.
                let kind = match reply {
                    Value::SimpleString(kind) => kind,
                    Value::BulkString(kind) => String::from_utf8_lossy(&kind).into_owned(),
                    _ => continue,
                };
                if kind != "none" {
                    keys.push(Key {
                        name: bytes_text(name),
                        kind,
                    });
                }
            }
        }
        keys.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Keys { keys, complete })
    }

    /// The databases that have keys, from INFO keyspace, and how many there
    /// are, from CONFIG GET databases. Managed services often refuse CONFIG,
    /// so the count then falls back to Redis's default of 16.
    pub async fn databases(&self) -> Result<Databases, Error> {
        let info =
            text(self.query(::redis::cmd("INFO").arg("keyspace")).await?).unwrap_or_default();
        let mut used: Vec<(u32, u64)> = info
            .lines()
            .filter_map(|line| {
                // db0:keys=3,expires=0,avg_ttl=0
                let (index, fields) = line.strip_prefix("db")?.split_once(':')?;
                let keys = fields
                    .split(',')
                    .find_map(|field| field.strip_prefix("keys="))?;
                Some((index.parse().ok()?, keys.trim().parse().ok()?))
            })
            .collect();
        used.sort_unstable();

        let count = match self
            .query(::redis::cmd("CONFIG").arg("GET").arg("databases"))
            .await
        {
            Ok(reply) => config_value(reply)
                .and_then(|count| count.parse().ok())
                .unwrap_or(DEFAULT_DATABASES),
            Err(Error::Database { .. }) => DEFAULT_DATABASES,
            Err(err) => return Err(err),
        };
        let count = used
            .iter()
            .map(|&(index, _)| index.saturating_add(1))
            .fold(count, u32::max);
        Ok(Databases { count, used })
    }

    /// Sends one command and returns its reply, with an error reply as an error.
    async fn query(&self, cmd: &Cmd) -> Result<Value, Error> {
        let mut connection = self.connection.clone();
        match connection.send_packed_command(cmd).await? {
            Value::ServerError(err) => Err(RedisError::from(err).into()),
            reply => Ok(reply),
        }
    }
}

/// The command that shows a key's value for its kind, quoted for
/// [`Session::execute`]; TYPE for kinds without one.
pub fn show_command(key: &Key) -> String {
    let name = quote(&key.name);
    match key.kind.as_str() {
        "string" => format!("GET {name}"),
        "hash" => format!("HGETALL {name}"),
        "list" => format!("LRANGE {name} 0 -1"),
        "set" => format!("SMEMBERS {name}"),
        "zset" => format!("ZRANGE {name} 0 -1 WITHSCORES"),
        "stream" => format!("XRANGE {name} - + COUNT 1000"),
        "ReJSON-RL" => format!("JSON.GET {name}"),
        _ => format!("TYPE {name}"),
    }
}

impl From<RedisError> for Error {
    fn from(err: RedisError) -> Self {
        // An error reply from the server, such as `WRONGTYPE Operation against
        // a key holding the wrong kind of value`.
        if let Some(code) = err.code() {
            return Error::Database {
                message: err
                    .detail()
                    .map(str::to_owned)
                    .unwrap_or_else(|| err.to_string()),
                code: code.to_owned(),
                detail: None,
                hint: None,
                position: None,
            };
        }
        match err.kind() {
            // Only reached on servers older than Redis 6, which log in with
            // AUTH and answer a wrong password with an ERR the library drops.
            ErrorKind::AuthenticationFailed => Error::Database {
                message: "invalid password".into(),
                code: "ERR".into(),
                detail: None,
                hint: None,
                position: None,
            },
            _ if closed(&err) => Error::Connection {
                message: "the connection to the server was closed".into(),
            },
            _ => Error::Connection {
                message: err.to_string(),
            },
        }
    }
}

/// Whether the server closed the connection, rather than refused it.
fn closed(err: &RedisError) -> bool {
    err.is_connection_dropped() && !err.is_connection_refusal()
}

/// Reads the database number in a connection's settings.
fn database_number(database: &str) -> Result<u32, Error> {
    let database = database.trim();
    if database.is_empty() {
        return Ok(0);
    }
    database.parse().map_err(|_| Error::Connection {
        message: format!("the database must be a number, such as 0, not {database:?}"),
    })
}

/// A plain or TLS stream, as one type for the multiplexer.
trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}

/// Connects and logs in with the first of `protocols` the server takes,
/// moving on only when it doesn't know HELLO.
async fn open(
    config: &ConnectConfig,
    database: u32,
    protocols: &[ProtocolVersion],
) -> Result<(MultiplexedConnection, ProtocolVersion), Error> {
    let attempt = async {
        let mut rejected = None;
        for &protocol in protocols {
            let stream = socket(config).await?;
            match handshake(stream, config, database, protocol).await {
                Ok(connection) => return Ok((connection, protocol)),
                Err(err) if rejects_hello(&err) => rejected = Some(err),
                Err(err) => return Err(handshake_error(err, config)),
            }
        }
        Err(handshake_error(
            rejected.expect("at least one protocol is tried"),
            config,
        ))
    };
    tokio::time::timeout(CONNECT_TIMEOUT, attempt)
        .await
        .map_err(|_| Error::Connection {
            message: format!("timed out connecting to {}:{}", config.host, config.port),
        })?
}

/// Opens the TCP connection, with TLS on top when the settings ask for it.
///
/// The TLS is set up here rather than by the redis crate so that, as for
/// PostgreSQL, `require` still sends the host name (SNI), which some
/// providers route on, while not verifying the certificate.
async fn socket(config: &ConnectConfig) -> Result<Box<dyn Socket>, Error> {
    let host = config.host.trim_start_matches('[').trim_end_matches(']');
    let tcp = tokio::net::TcpStream::connect((host, config.port))
        .await
        .map_err(|err| Error::Connection {
            message: format!("couldn't connect to {}:{}: {err}", config.host, config.port),
        })?;
    // Commands are small and each waits for its reply.
    let _ = tcp.set_nodelay(true);
    if matches!(config.ssl_mode, SslMode::Disable | SslMode::Prefer) {
        return Ok(Box::new(tcp));
    }

    let verify = config.ssl_mode == SslMode::VerifyFull;
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(!verify)
        .danger_accept_invalid_hostnames(!verify)
        .build()
        .map_err(|err| Error::Connection {
            message: err.to_string(),
        })?;
    let tls = tokio_native_tls::TlsConnector::from(connector)
        .connect(host, tcp)
        .await
        .map_err(|err| Error::Connection {
            message: format!("TLS handshake failed: {err}"),
        })?;
    Ok(Box::new(tls))
}

/// Logs in and selects the database, then starts the multiplexer's task.
async fn handshake(
    stream: Box<dyn Socket>,
    config: &ConnectConfig,
    database: u32,
    protocol: ProtocolVersion,
) -> Result<MultiplexedConnection, RedisError> {
    let mut login = RedisConnectionInfo::default()
        .set_protocol(protocol)
        .set_db(database.into());
    if !config.user.is_empty() {
        // An ACL user may have no password (`nopass`), but still has to log in by name.
        login = login
            .set_username(&config.user)
            .set_password(config.password.as_deref().unwrap_or_default());
    } else if let Some(password) = config.password.as_deref().filter(|p| !p.is_empty()) {
        login = login.set_password(password);
    }
    // Commands may take long, such as KEYS on a big database, so replies
    // aren't timed out. The library's default is half a second.
    let settings = AsyncConnectionConfig::new().set_response_timeout(None);
    let (connection, driver) =
        MultiplexedConnection::new_with_config(&login, stream, settings).await?;
    // The task ends once every clone of the connection is dropped.
    tokio::spawn(driver);
    Ok(connection)
}

/// Whether the server turned down HELLO for not knowing it, as before Redis 6.
fn rejects_hello(err: &RedisError) -> bool {
    err.kind() == ErrorKind::RESP3NotSupported
        || (err.code() == Some("ERR")
            && err.detail().is_some_and(|detail| {
                let detail = detail.to_ascii_lowercase();
                detail.contains("unknown command") && detail.contains("hello")
            }))
}

/// A login error, with a hint when a TLS-only server hung up on plain text.
fn handshake_error(err: RedisError, config: &ConnectConfig) -> Error {
    if closed(&err) && matches!(config.ssl_mode, SslMode::Disable | SslMode::Prefer) {
        return Error::Connection {
            message: format!(
                "{}:{} closed the connection while logging in; it may require TLS",
                config.host, config.port
            ),
        };
    }
    err.into()
}

/// Refuses commands that would break the connection everything shares, or
/// change its state under the other users.
fn refuse_unsupported(args: &[Vec<u8>]) -> Result<(), Error> {
    let name = upper(&args[0]);
    let message = match name.as_str() {
        "SELECT" => {
            "SELECT would switch the database under everything sharing this connection. Choose the database in the database list instead.".to_owned()
        }
        "MULTI" | "EXEC" | "WATCH" => format!(
            "{name} isn't supported here: other commands share this connection and could land inside the transaction. Use a Lua script (EVAL) to run commands atomically."
        ),
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR" => format!(
            "{name} would take the connection over to wait for messages, blocking every other command."
        ),
        "SYNC" | "PSYNC" => {
            format!("{name} would turn the connection into a replication stream.")
        }
        "QUIT" => "QUIT would close the connection. Disconnect in the app instead.".to_owned(),
        "RESET" => {
            "RESET would reset the database and login of the connection everything shares."
                .to_owned()
        }
        "HELLO" => {
            "HELLO would change the protocol and login of the connection everything shares."
                .to_owned()
        }
        "CLIENT" if args.get(1).is_some_and(|sub| sub.eq_ignore_ascii_case(b"reply")) => {
            "CLIENT REPLY would turn off the replies the app waits for, and hang the connection."
                .to_owned()
        }
        _ => return Ok(()),
    };
    Err(Error::Refused {
        reason: Refusal::Unsupported,
        message,
    })
}

/// Commands that take a subcommand, to name both in messages.
fn is_container(name: &str) -> bool {
    matches!(
        name,
        "CONFIG" | "CLIENT" | "ACL" | "FUNCTION" | "SCRIPT" | "MODULE"
    )
}

/// Reads a COMMAND INFO reply: `[name, arity, flags, ..., subcommands]`, or
/// nil for a command the server doesn't know.
fn command_info(reply: Value) -> Option<CommandInfo> {
    let Value::Array(mut entries) = reply else {
        return None;
    };
    let entry = entries.pop()?;
    let (_, writes, subcommand_entries) = command_entry(entry)?;
    let subcommands = subcommand_entries
        .into_iter()
        .filter_map(command_entry)
        .filter_map(|(name, writes, _)| {
            // Subcommands are named "config|set".
            let (_, sub) = name.split_once('|')?;
            Some((sub.to_ascii_lowercase(), writes))
        })
        .collect();
    Some(CommandInfo {
        writes,
        subcommands,
    })
}

/// One COMMAND INFO entry: its name, whether it's flagged `write`, and its
/// subcommands' entries.
fn command_entry(entry: Value) -> Option<(String, bool, Vec<Value>)> {
    let Value::Array(fields) = entry else {
        return None;
    };
    let mut fields = fields.into_iter();
    let name = text(fields.next()?)?;
    let _arity = fields.next()?;
    let writes = match fields.next()? {
        Value::Array(flags) | Value::Set(flags) => flags
            .into_iter()
            .filter_map(text)
            .any(|flag| flag.eq_ignore_ascii_case("write")),
        _ => false,
    };
    // first key, last key, step, ACL categories, tips, key specs, then subcommands.
    let subcommands = match fields.nth(6) {
        Some(Value::Array(subcommands)) => subcommands,
        _ => Vec::new(),
    };
    Some((name, writes, subcommands))
}

/// The cursor and keys of a SCAN reply.
fn scan_reply(reply: Value) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let Value::Array(parts) = reply else {
        return None;
    };
    let mut parts = parts.into_iter();
    let cursor = match parts.next()? {
        Value::BulkString(cursor) => cursor,
        Value::SimpleString(cursor) => cursor.into_bytes(),
        Value::Int(cursor) => cursor.to_string().into_bytes(),
        _ => return None,
    };
    let Value::Array(names) = parts.next()? else {
        return None;
    };
    let names = names
        .into_iter()
        .filter_map(|name| match name {
            Value::BulkString(name) => Some(name),
            Value::SimpleString(name) => Some(name.into_bytes()),
            _ => None,
        })
        .collect();
    Some((cursor, names))
}

/// The value in a CONFIG GET reply for one parameter: a map in RESP3, a
/// `[name, value]` array in RESP2.
fn config_value(reply: Value) -> Option<String> {
    match reply {
        Value::Map(entries) => entries
            .into_iter()
            .next()
            .and_then(|(_, value)| text(value)),
        Value::Array(items) => items.into_iter().nth(1).and_then(text),
        _ => None,
    }
}

/// A reply's text, for replies that are strings.
fn text(value: Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => Some(text),
        Value::Okay => Some("OK".into()),
        _ => None,
    }
}

fn upper(arg: &[u8]) -> String {
    String::from_utf8_lossy(arg).to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use command::split_arguments;

    /// Parses a reply as it comes over the wire.
    fn reply(wire: impl AsRef<[u8]>) -> Value {
        ::redis::parse_redis_value(wire.as_ref()).unwrap()
    }

    #[test]
    fn shows_each_kind() {
        let key = |kind: &str| Key {
            name: "my key".into(),
            kind: kind.into(),
        };
        assert_eq!(show_command(&key("string")), r#"GET "my key""#);
        assert_eq!(show_command(&key("hash")), r#"HGETALL "my key""#);
        assert_eq!(show_command(&key("list")), r#"LRANGE "my key" 0 -1"#);
        assert_eq!(show_command(&key("set")), r#"SMEMBERS "my key""#);
        assert_eq!(
            show_command(&key("zset")),
            r#"ZRANGE "my key" 0 -1 WITHSCORES"#
        );
        assert_eq!(
            show_command(&key("stream")),
            r#"XRANGE "my key" - + COUNT 1000"#
        );
        assert_eq!(show_command(&key("ReJSON-RL")), r#"JSON.GET "my key""#);
        assert_eq!(show_command(&key("vectorset")), r#"TYPE "my key""#);
    }

    #[test]
    fn maps_server_errors_with_their_code() {
        let err = RedisError::from(
            match reply("-WRONGTYPE Operation against a key holding the wrong kind of value\r\n") {
                Value::ServerError(err) => err,
                other => panic!("expected an error, got {other:?}"),
            },
        );
        match Error::from(err) {
            Error::Database { message, code, .. } => {
                assert_eq!(code, "WRONGTYPE");
                assert_eq!(
                    message,
                    "Operation against a key holding the wrong kind of value"
                );
            }
            other => panic!("expected a database error, got {other:?}"),
        }
        let io = RedisError::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert!(matches!(Error::from(io), Error::Connection { .. }));
    }

    #[test]
    fn reads_database_numbers() {
        assert_eq!(database_number("").unwrap(), 0);
        assert_eq!(database_number(" 3 ").unwrap(), 3);
        assert!(matches!(
            database_number("app"),
            Err(Error::Connection { .. })
        ));
    }

    #[test]
    fn refuses_commands_that_break_the_shared_connection() {
        for line in [
            "select 1",
            "MULTI",
            "exec",
            "WATCH k",
            "SUBSCRIBE c",
            "monitor",
            "PSYNC ? -1",
            "QUIT",
            "RESET",
            "HELLO 2",
            "CLIENT REPLY OFF",
        ] {
            let args = split_arguments(line).unwrap();
            assert!(
                matches!(
                    refuse_unsupported(&args),
                    Err(Error::Refused {
                        reason: Refusal::Unsupported,
                        ..
                    })
                ),
                "{line}"
            );
        }
        for line in ["GET k", "CLIENT LIST", "DISCARD"] {
            assert!(
                refuse_unsupported(&split_arguments(line).unwrap()).is_ok(),
                "{line}"
            );
        }
    }

    #[test]
    fn reads_command_info() {
        // RESP3, Redis 7: CONFIG with its subcommands.
        let info = command_info(reply(concat!(
            "*1\r\n*10\r\n$6\r\nconfig\r\n:-2\r\n~0\r\n:0\r\n:0\r\n:0\r\n*0\r\n*0\r\n*0\r\n",
            "*2\r\n",
            "*10\r\n$10\r\nconfig|set\r\n:-4\r\n~2\r\n+admin\r\n+noscript\r\n:0\r\n:0\r\n:0\r\n*0\r\n*0\r\n*0\r\n*0\r\n",
            "*10\r\n$10\r\nconfig|get\r\n:-3\r\n~1\r\n+admin\r\n:0\r\n:0\r\n:0\r\n*0\r\n*0\r\n*0\r\n*0\r\n",
        )))
        .unwrap();
        assert!(!info.writes);
        assert_eq!(info.subcommands.get("set"), Some(&false));
        assert_eq!(info.subcommands.len(), 2);

        // RESP2, Redis 6: no subcommands.
        let info = command_info(reply(
            "*1\r\n*7\r\n$3\r\nset\r\n:-3\r\n*2\r\n+write\r\n+denyoom\r\n:1\r\n:1\r\n:1\r\n*0\r\n",
        ))
        .unwrap();
        assert!(info.writes);
        assert!(info.subcommands.is_empty());

        assert!(command_info(reply("*1\r\n_\r\n")).is_none());
    }
}
