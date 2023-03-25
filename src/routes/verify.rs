use crate::{
    compile,
    provider::{contract_runtime_code, MultiChainProvider},
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ethers::types::{Address, BlockId, Bytes, Chain, TxHash};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};
use tempfile::TempDir;

#[derive(Deserialize)]
pub struct VerifyData {
    repo_url: String,
    repo_commit: String,
    contract_address: String,
}

#[derive(Serialize)]
pub struct CompilerInfo {
    name: String,
    version: String,
    // settings: String,
    // input: String,
    // output: String,
}

#[derive(Serialize)]
struct SuccessfulVerification {
    repo_url: String,
    repo_commit: String,
    contract_address: Address,
    chains: Vec<Chain>, // All chains this address has verified code on.
    chain: Chain,       // The chain data is being returned for.
    creation_tx_hash: TxHash,
    creation_block_number: u64,
    creation_code: Bytes,
    runtime_code: Bytes,
    creation_code_source_map: String,
    runtime_code_source_map: String,
    abi: Vec<Abi>,
    compiler_info: CompilerInfo,
    ast: Ast,
}

#[tracing::instrument(
    name = "Verifying contract",
    skip(json),
    fields(
        repo_url = %json.repo_url,
        repo_commit = %json.repo_commit,
        contract_address = %json.contract_address,
    )
)]
pub async fn verify(Json(json): Json<VerifyData>) -> Response {
    let repo_url = json.repo_url.as_str();
    let commit_hash = json.repo_commit.as_str();
    let contract_addr = Address::from_str(json.contract_address.as_str()).unwrap();
    println!("VERIFICATION INPUTS:");
    println!("  Repo URL:         {}", repo_url);
    println!("  Commit Hash:      {}", commit_hash);
    println!("  Contract Address: {:?}", contract_addr);

    println!("\nFETCHING CREATION CODE");
    let provider = MultiChainProvider::default();
    let creation_data = provider.get_creation_code(contract_addr).await;

    // Return an error if there's no creation code for the transaction hash.
    if creation_data.is_all_none() {
        println!("  Creation code not found, returning error.");
        let msg = format!("No creation code for {:?} found on any supported chain", contract_addr);
        return (StatusCode::BAD_REQUEST, msg).into_response()
    }
    println!("  Found creation code on the following chains: {:?}", creation_data.responses.keys());

    // Create a temporary directory for the cloned repository.
    let temp_dir = TempDir::new().unwrap();
    let path = &temp_dir.path();

    // Clone the repository and checking out the commit.
    println!("\nCLONING REPOSITORY");
    let maybe_repo = clone_repo_and_checkout_commit(repo_url, commit_hash, &temp_dir).await;
    if maybe_repo.is_err() {
        println!("  Unable to clone repository, returning error.");
        return (StatusCode::BAD_REQUEST, format!("Unable to clone repository {repo_url}"))
            .into_response()
    }
    println!("  Repository cloned successfully.");

    // Get the build commands for the project.
    println!("\nBUILDING CONTRACTS AND COMPARING BYTECODE");
    let build_commands = compile::build_commands(path).unwrap();
    let mut verified_contracts: HashMap<Chain, PathBuf> = HashMap::new();

    for mut build_command in build_commands {
        println!("  Building with command: {}", format!("{:?}", build_command).replace("\"", ""));

        // Build the contracts.
        std::env::set_current_dir(path).unwrap();
        let build_result = build_command.output().unwrap();
        if !build_result.status.success() {
            println!("    Build failed, continuing to next build command.");
            continue // This profile might not compile, e.g. perhaps it fails with stack too deep.
        }
        println!("    Build succeeded, comparing creation code.");

        let artifacts = compile::get_artifacts(Path::join(path, "out")).unwrap();
        let matches = provider.compare_creation_code(artifacts, &creation_data);

        if matches.is_all_none() {
            println!("    No matching contracts found, continuing to next build command.");
        }

        // If two profiles match, we overwrite the first with the second. This is ok, because solc
        // inputs to outputs are not necessarily 1:1, e.g. changing optimization settings may not
        // change bytecode. This is likely true for other compilers too.
        for (chain, path) in matches.iter_entries() {
            // Extract contract name from path by removing the extension
            let stem = path.file_stem().unwrap();
            println!("    ✅ Found matching contract on chain {:?}: {:?}", chain, stem);
            verified_contracts.insert(*chain, path.clone());
        }
    }

    if verified_contracts.is_empty() {
        return (StatusCode::BAD_REQUEST, "No matching contracts found".to_string()).into_response()
    }

    // If multiple matches found, tell user we are choosing one.
    if verified_contracts.len() > 1 {
        println!("\nCONTRACT VERIFICATION SUCCESSFUL!");
        println!("\nPREPARING RESPONSE");
        println!("  Multiple matching contracts found, choosing Optimism arbitrarily.");
    }

    // Format response. If there are multiple chains we verified on, we just return an arbitrary one
    // for now. For now we just hardcode Optimism for demo purposes.
    let artifact_path = verified_contracts.get(&Chain::Optimism).unwrap();
    let artifact_content = fs::read_to_string(artifact_path).unwrap();
    let artifact: Root = serde_json::from_str(&artifact_content).unwrap();

    let compiler_info = CompilerInfo {
        name: artifact.metadata.language,
        version: artifact.metadata.compiler.version,
    };

    let block = creation_data.responses.get(&Chain::Optimism).unwrap().as_ref().unwrap().block;
    let block_num = match block {
        BlockId::Number(num) => num.as_number().unwrap(),
        BlockId::Hash(_) => todo!(),
    };

    let selected_creation_data =
        creation_data.responses.get(&Chain::Optimism).unwrap().as_ref().unwrap();

    let response = SuccessfulVerification {
        repo_url: repo_url.to_string(),
        repo_commit: commit_hash.to_string(),
        contract_address: contract_addr,
        chains: verified_contracts.keys().copied().collect(),
        chain: Chain::Optimism, // TODO Un-hardcode this
        creation_tx_hash: selected_creation_data.tx_hash,
        creation_block_number: block_num.as_u64(),
        creation_code: selected_creation_data.creation_code.clone(),
        runtime_code: contract_runtime_code(
            provider.providers.get(&Chain::Optimism).unwrap(),
            contract_addr,
        )
        .await,
        creation_code_source_map: artifact.bytecode.source_map,
        runtime_code_source_map: artifact.deployed_bytecode.source_map,
        abi: artifact.abi,
        compiler_info,
        ast: artifact.ast,
    };

    (StatusCode::OK, Json(response)).into_response()
}

async fn clone_repo_and_checkout_commit(
    repo_url: &str,
    commit_hash: &str,
    temp_dir: &TempDir,
) -> Result<Repository, Box<dyn Error + Send + Sync>> {
    // Clone the repository.
    let repo = Repository::clone(repo_url, temp_dir.path())?;
    println!("  Repository cloned.");

    // Find the specified commit (object ID).
    let oid = Oid::from_str(commit_hash)?;
    let commit = repo.find_commit(oid)?;

    // Create a branch for the commit.
    let branch = repo.branch(commit_hash, &commit, false);

    // Checkout the commit.
    let obj = repo.revparse_single(&("refs/heads/".to_owned() + commit_hash)).unwrap();
    repo.checkout_tree(&obj, None)?;

    repo.set_head(&("refs/heads/".to_owned() + commit_hash))?;
    println!("  Checked out specified commit.");

    // Drop objects that have references to the repo so that we can return it.
    drop(branch);
    drop(commit);
    drop(obj);
    Ok(repo)
}

// ======================================
// ======== Forge Artifact Types ========
// ======================================
// These were auto-generated by pasting an artifact into https://transform.tools/json-to-rust-serde.

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub abi: Vec<Abi>,
    pub bytecode: Bytecode,
    pub deployed_bytecode: DeployedBytecode,
    pub method_identifiers: MethodIdentifiers,
    pub raw_metadata: String,
    pub metadata: Metadata,
    pub ast: Ast,
    pub id: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Abi {
    pub inputs: Vec<Input>,
    pub name: String,
    pub outputs: Vec<Output>,
    pub state_mutability: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub internal_type: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub internal_type: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bytecode {
    pub object: String,
    pub source_map: String,
    pub link_references: LinkReferences,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReferences {}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployedBytecode {
    pub object: String,
    pub source_map: String,
    pub link_references: LinkReferences2,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReferences2 {}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodIdentifiers {
    #[serde(rename = "increment()")]
    pub increment: String,
    #[serde(rename = "number()")]
    pub number: String,
    #[serde(rename = "setNumber(uint256)")]
    pub set_number_uint256: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub compiler: Compiler,
    pub language: String,
    pub output: Output2,
    pub settings: Settings,
    pub sources: Sources,
    pub version: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Compiler {
    pub version: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output2 {
    pub abi: Vec<Abi2>,
    pub devdoc: Devdoc,
    pub userdoc: Userdoc,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Abi2 {
    pub inputs: Vec<Input2>,
    pub state_mutability: String,
    #[serde(rename = "type")]
    pub type_field: String,
    pub name: String,
    #[serde(default)]
    pub outputs: Vec<Output3>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input2 {
    pub internal_type: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output3 {
    pub internal_type: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Devdoc {
    pub kind: String,
    pub methods: Methods,
    pub version: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Methods {}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Userdoc {
    pub kind: String,
    pub methods: Methods2,
    pub version: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Methods2 {}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub remappings: Vec<String>,
    pub optimizer: Optimizer,
    pub metadata: Metadata2,
    pub compilation_target: CompilationTarget,
    pub libraries: Libraries,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Optimizer {
    pub enabled: bool,
    pub runs: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata2 {
    pub bytecode_hash: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompilationTarget {
    #[serde(rename = "src/Counter.sol")]
    pub src_counter_sol: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Libraries {}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sources {
    #[serde(rename = "src/Counter.sol")]
    pub src_counter_sol: SrcCounterSol,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SrcCounterSol {
    pub keccak256: String,
    pub urls: Vec<String>,
    pub license: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ast {
    pub absolute_path: String,
    pub id: i64,
    pub exported_symbols: ExportedSymbols,
    pub node_type: String,
    pub src: String,
    pub nodes: Vec<Node>,
    pub license: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedSymbols {
    #[serde(rename = "Counter")]
    pub counter: Vec<i64>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: i64,
    pub node_type: String,
    pub src: String,
    pub nodes: Vec<Node2>,
    pub literals: Option<Vec<String>>,
    #[serde(rename = "abstract")]
    pub abstract_field: Option<bool>,
    #[serde(default)]
    pub base_contracts: Vec<Value>,
    pub canonical_name: Option<String>,
    #[serde(default)]
    pub contract_dependencies: Vec<Value>,
    pub contract_kind: Option<String>,
    pub fully_implemented: Option<bool>,
    #[serde(default)]
    pub linearized_base_contracts: Vec<i64>,
    pub name: Option<String>,
    pub name_location: Option<String>,
    pub scope: Option<i64>,
    #[serde(default)]
    pub used_errors: Vec<Value>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node2 {
    pub id: i64,
    pub node_type: String,
    pub src: String,
    pub nodes: Vec<Value>,
    pub constant: Option<bool>,
    pub function_selector: String,
    pub mutability: Option<String>,
    pub name: String,
    pub name_location: String,
    pub scope: i64,
    pub state_variable: Option<bool>,
    pub storage_location: Option<String>,
    pub type_descriptions: Option<TypeDescriptions>,
    pub type_name: Option<TypeName>,
    pub visibility: String,
    pub body: Option<Body>,
    pub implemented: Option<bool>,
    pub kind: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<Value>,
    pub parameters: Option<Parameters>,
    pub return_parameters: Option<ReturnParameters>,
    pub state_mutability: Option<String>,
    #[serde(rename = "virtual")]
    pub virtual_field: Option<bool>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeName {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub src: String,
    pub type_descriptions: TypeDescriptions2,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions2 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    pub id: i64,
    pub node_type: String,
    pub src: String,
    pub nodes: Vec<Value>,
    pub statements: Vec<Statement>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    pub expression: Expression,
    pub id: i64,
    pub node_type: String,
    pub src: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    pub id: i64,
    pub is_constant: bool,
    #[serde(rename = "isLValue")]
    pub is_lvalue: bool,
    pub is_pure: bool,
    pub l_value_requested: bool,
    pub node_type: String,
    pub operator: String,
    pub prefix: Option<bool>,
    pub src: String,
    pub sub_expression: Option<SubExpression>,
    pub type_descriptions: TypeDescriptions4,
    pub left_hand_side: Option<LeftHandSide>,
    pub right_hand_side: Option<RightHandSide>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubExpression {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub overloaded_declarations: Vec<Value>,
    pub referenced_declaration: i64,
    pub src: String,
    pub type_descriptions: TypeDescriptions3,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions3 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions4 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftHandSide {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub overloaded_declarations: Vec<Value>,
    pub referenced_declaration: i64,
    pub src: String,
    pub type_descriptions: TypeDescriptions5,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions5 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightHandSide {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub overloaded_declarations: Vec<Value>,
    pub referenced_declaration: i64,
    pub src: String,
    pub type_descriptions: TypeDescriptions6,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions6 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameters {
    pub id: i64,
    pub node_type: String,
    pub parameters: Vec<Parameter>,
    pub src: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub constant: bool,
    pub id: i64,
    pub mutability: String,
    pub name: String,
    pub name_location: String,
    pub node_type: String,
    pub scope: i64,
    pub src: String,
    pub state_variable: bool,
    pub storage_location: String,
    pub type_descriptions: TypeDescriptions7,
    pub type_name: TypeName2,
    pub visibility: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions7 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeName2 {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub src: String,
    pub type_descriptions: TypeDescriptions8,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDescriptions8 {
    pub type_identifier: String,
    pub type_string: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnParameters {
    pub id: i64,
    pub node_type: String,
    pub parameters: Vec<Value>,
    pub src: String,
}
