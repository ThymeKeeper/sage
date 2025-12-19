use crate::kernel::{ExecutionOutput, ExecutionResult, Kernel, KernelInfo, KernelType};
use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Direct Python kernel using subprocess communication
pub struct DirectKernel {
    info: KernelInfo,
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    execution_count: usize,
}

impl DirectKernel {
    pub fn new(python_path: String, name: String, display_name: String) -> Self {
        DirectKernel {
            info: KernelInfo {
                name,
                display_name,
                python_path,
                kernel_type: KernelType::Direct,
            },
            process: None,
            stdin: None,
            stdout: None,
            execution_count: 0,
        }
    }

    /// Create a Python REPL script that handles execution
    fn get_repl_script() -> &'static str {
        r#"
import sys
import traceback
import json
import os
import io
import contextlib

# Ensure we're not in interactive mode
sys.ps1 = sys.ps2 = ''

# Disable output buffering (handle older Python versions)
try:
    sys.stdout.reconfigure(line_buffering=True)
    sys.stderr.reconfigure(line_buffering=True)
except (AttributeError, OSError):
    # Python < 3.7 or when reconfigure fails
    pass

# Ensure TERM is set to dumb to avoid escape codes
os.environ['TERM'] = 'dumb'

print("SAGE_KERNEL_READY", flush=True)

while True:
    try:
        # Read delimiter
        line = input()
        if line != "SAGE_EXEC_START":
            continue

        # Read code until END delimiter
        code_lines = []
        while True:
            line = input()
            if line == "SAGE_EXEC_END":
                break
            code_lines.append(line)

        code = '\n'.join(code_lines)

        # Debug: Mark code received
        with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
            debug_f.write(f'>>> RECEIVED CODE ({len(code)} chars): {code[:50]}...\n')

        # Execute code with stdout capture
        # Use Jupyter-style execution: try eval, then try exec with last expression
        stdout_capture = io.StringIO()
        _sage_result = None

        try:
            # First, try to eval the entire code (for simple expressions)
            with contextlib.redirect_stdout(stdout_capture):
                _sage_result = eval(code, globals())
            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                debug_f.write(f'>>> EVAL succeeded\n')
        except SyntaxError:
            # If eval fails, just exec the entire code block
            with contextlib.redirect_stdout(stdout_capture):
                exec(code, globals())
            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                debug_f.write(f'>>> EXEC succeeded\n')

        # Send captured stdout if any
        captured = stdout_capture.getvalue()
        if captured:
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "stdout", "data": captured}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)

        # Collect namespace completions for autocomplete
        # IMPORTANT: Send completions BEFORE the success/result marker

        # Debug marker - write directly to file
        with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
            debug_f.write('=== INTROSPECTION START ===\n')

        try:
            completions = []
            return_types = {}  # Maps callable names to their return types
            type_methods = {}  # Maps type names to their methods
            sql_tables = []    # SQL table names
            sql_columns = []   # SQL column names (format: "table.column")
            sql_functions = [] # SQL function names

            # Debug: Check what's in globals
            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                all_names = list(globals().keys())
                debug_f.write(f'Globals count: {len(all_names)}\n')
                debug_f.write(f'Has db: {"db" in globals()}\n')
                debug_f.write(f'Has duckdb: {"duckdb" in globals()}\n')
                debug_f.write(f'First 10 names: {all_names[:10]}\n')

            # Take a snapshot of globals to avoid "dictionary changed size during iteration"
            globals_snapshot = dict(globals())

            # Get all names from globals snapshot
            for name in globals_snapshot:
                # Skip private/internal names
                if name.startswith('_') or name.startswith('SAGE_'):
                    continue

                obj = globals_snapshot[name]
                obj_type = type(obj).__name__

                # Check if it's a module
                if obj_type == 'module':
                    # Add module name
                    completions.append({"name": name, "type": "module"})

                    # Add module members (functions, classes, constants)
                    try:
                        members = dir(obj)
                        for member in members:
                            if not member.startswith('_'):
                                try:
                                    member_obj = getattr(obj, member)
                                    member_type = type(member_obj).__name__
                                    full_name = f"{name}.{member}"

                                    # Add as "module.member"
                                    completions.append({
                                        "name": full_name,
                                        "type": member_type
                                    })

                                    # Try to get return type for functions/methods
                                    if callable(member_obj):
                                        try:
                                            # Check for type hints
                                            import typing
                                            import inspect
                                            sig = inspect.signature(member_obj)
                                            if sig.return_annotation != inspect.Parameter.empty:
                                                # Get the return type name
                                                return_type = sig.return_annotation
                                                if hasattr(return_type, '__name__'):
                                                    return_type_name = return_type.__name__
                                                else:
                                                    return_type_name = str(return_type).split('.')[-1].rstrip("'>")
                                                return_types[full_name] = return_type_name
                                        except:
                                            pass

                                    # If it's a class/type, introspect its methods NOW (even if no instances exist)
                                    if member_type in ['type', 'ABCMeta', 'pybind11_type']:
                                        try:
                                            # Get the actual type name
                                            type_name = member_obj.__name__ if hasattr(member_obj, '__name__') else member

                                            # Debug: Log type discovery
                                            if name == 'db':  # Only for duckdb module
                                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                                    debug_f.write(f'Found type in db: {member} (member_type={member_type}, type_name={type_name})\n')

                                            if type_name not in type_methods:
                                                class_methods = []
                                                for method_name in dir(member_obj):
                                                    if not method_name.startswith('_'):
                                                        class_methods.append(method_name)
                                                if class_methods:
                                                    type_methods[type_name] = class_methods
                                                    if name == 'db':
                                                        with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                                            debug_f.write(f'  -> Added {len(class_methods)} methods for {type_name}\n')

                                            # For callable classes, try to determine what they return
                                            # Many C extension functions return instances of types in the same module
                                            if callable(member_obj) and member_type in ['type', 'ABCMeta', 'pybind11_type']:
                                                # If it's a callable type (constructor), it returns instances of itself
                                                return_types[full_name] = type_name
                                        except Exception as e:
                                            if name == 'db':
                                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                                    debug_f.write(f'Error introspecting {member}: {e}\n')
                                except:
                                    pass
                    except:
                        pass
                elif obj_type in ['function', 'builtin_function_or_method', 'type', 'ABCMeta']:
                    # User-defined or built-in functions and classes
                    completions.append({"name": name, "type": obj_type})

                    # Try to get return type for functions
                    if callable(obj) and obj_type in ['function', 'builtin_function_or_method']:
                        try:
                            import inspect
                            sig = inspect.signature(obj)
                            if sig.return_annotation != inspect.Parameter.empty:
                                return_type = sig.return_annotation
                                if hasattr(return_type, '__name__'):
                                    return_type_name = return_type.__name__
                                else:
                                    return_type_name = str(return_type).split('.')[-1].rstrip("'>")
                                return_types[name] = return_type_name
                        except:
                            pass
                else:
                    # Variables (includes DataFrames, Series, etc.)
                    completions.append({"name": name, "type": obj_type})

                    # Introspect the type to get its methods
                    try:
                        if obj_type not in type_methods:
                            type_instance_methods = []
                            members = dir(obj)
                            for member in members:
                                if not member.startswith('_'):
                                    try:
                                        member_obj = getattr(obj, member)
                                        type_instance_methods.append(member)

                                        # If the member is callable, try to get its return type
                                        if callable(member_obj):
                                            try:
                                                import inspect
                                                sig = inspect.signature(member_obj)
                                                if sig.return_annotation != inspect.Parameter.empty:
                                                    return_type = sig.return_annotation
                                                    if hasattr(return_type, '__name__'):
                                                        return_type_name = return_type.__name__
                                                    else:
                                                        return_type_name = str(return_type).split('.')[-1].rstrip("'>")
                                                    return_types[f"{obj_type}.{member}"] = return_type_name
                                            except:
                                                pass
                                    except:
                                        pass
                            if type_instance_methods:
                                type_methods[obj_type] = type_instance_methods

                        # Also add completions for object.member pattern
                        members = dir(obj)
                        for member in members:
                            if not member.startswith('_'):
                                try:
                                    member_obj = getattr(obj, member)
                                    member_type = type(member_obj).__name__
                                    # Add as "variable.method" or "variable.attribute"
                                    completions.append({
                                        "name": f"{name}.{member}",
                                        "type": member_type
                                    })
                                except:
                                    pass
                    except:
                        pass

            # Debug: Write completion summary to file
            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                debug_f.write(f'Completions collected: {len(completions)}\n')
                if completions:
                    sample = [c['name'] for c in completions[:5]]
                    debug_f.write(f'Sample: {sample}\n')
                    # Check for 'db' specifically
                    db_items = [c['name'] for c in completions if c['name'].startswith('db')]
                    debug_f.write(f'DB items found: {len(db_items)}\n')
                    if db_items:
                        debug_f.write(f'DB items: {db_items[:10]}\n')
                debug_f.write(f'Type methods keys: {list(type_methods.keys())[:5]}\n')

            # Harvest SQL metadata from DuckDB and Spark connections
            for name in globals_snapshot:
                if name.startswith('_') or name.startswith('SAGE_'):
                    continue

                try:
                    obj = globals_snapshot[name]
                    obj_type = type(obj).__name__

                    # Check if this is the duckdb module itself
                    if obj_type == 'module' and hasattr(obj, '__name__') and obj.__name__ == 'duckdb':
                        try:
                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Found duckdb module: {name}\n')

                            # Use the module's default connection via execute()
                            try:
                                tables_result = obj.execute("SHOW TABLES").fetchall()
                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                    debug_f.write(f'SHOW TABLES returned: {tables_result}\n')
                                for row in tables_result:
                                    table_name = row[0]
                                    if table_name not in sql_tables:
                                        sql_tables.append(table_name)

                                    # Get columns for this table
                                    try:
                                        columns_result = obj.execute(f"DESCRIBE {table_name}").fetchall()
                                        for col_row in columns_result:
                                            col_name = col_row[0]
                                            # Add fully qualified name (table.column)
                                            full_name = f"{table_name}.{col_name}"
                                            if full_name not in sql_columns:
                                                sql_columns.append(full_name)
                                            # Also add unqualified name (just column)
                                            if col_name not in sql_columns:
                                                sql_columns.append(col_name)
                                    except Exception as col_e:
                                        with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                            debug_f.write(f'Error getting columns for {table_name}: {str(col_e)}\n')
                            except Exception as table_e:
                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                    debug_f.write(f'Error with SHOW TABLES: {str(table_e)}\n')

                            # Get functions (only once)
                            if not sql_functions:
                                try:
                                    functions_result = obj.execute("SELECT DISTINCT function_name FROM duckdb_functions() ORDER BY function_name").fetchall()
                                    for func_row in functions_result:
                                        sql_functions.append(func_row[0])
                                except Exception as func_e:
                                    with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                        debug_f.write(f'Error getting functions: {str(func_e)}\n')

                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'DuckDB module SQL metadata: {len(sql_tables)} tables, {len(sql_columns)} columns, {len(sql_functions)} functions\n')
                                if sql_tables:
                                    debug_f.write(f'Tables: {sql_tables}\n')
                        except Exception as e:
                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Error harvesting from duckdb module: {str(e)}\n')
                                import traceback
                                debug_f.write(f'Traceback: {traceback.format_exc()}\n')

                    # Check for DuckDB connection object
                    elif obj_type == 'DuckDBPyConnection':
                        try:
                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Found DuckDB connection: {name}\n')

                            # Get tables - use SHOW TABLES which is more reliable
                            try:
                                tables_result = obj.execute("SHOW TABLES").fetchall()
                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                    debug_f.write(f'SHOW TABLES returned: {tables_result}\n')
                                for row in tables_result:
                                    table_name = row[0]
                                    if table_name not in sql_tables:
                                        sql_tables.append(table_name)

                                    # Get columns for this table
                                    try:
                                        columns_result = obj.execute(f"DESCRIBE {table_name}").fetchall()
                                        for col_row in columns_result:
                                            col_name = col_row[0]  # First column is column name
                                            # Add fully qualified name (table.column)
                                            full_name = f"{table_name}.{col_name}"
                                            if full_name not in sql_columns:
                                                sql_columns.append(full_name)
                                            # Also add unqualified name (just column)
                                            if col_name not in sql_columns:
                                                sql_columns.append(col_name)
                                    except Exception as col_e:
                                        with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                            debug_f.write(f'Error getting columns for {table_name}: {str(col_e)}\n')
                            except Exception as table_e:
                                with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                    debug_f.write(f'Error with SHOW TABLES: {str(table_e)}\n')

                            # Get functions (only once, not per table)
                            if not sql_functions:  # Only populate if empty
                                try:
                                    functions_result = obj.execute("SELECT DISTINCT function_name FROM duckdb_functions() ORDER BY function_name").fetchall()
                                    for func_row in functions_result:
                                        sql_functions.append(func_row[0])
                                except Exception as func_e:
                                    with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                        debug_f.write(f'Error getting functions: {str(func_e)}\n')

                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'DuckDB SQL metadata: {len(sql_tables)} tables, {len(sql_columns)} columns, {len(sql_functions)} functions\n')
                                if sql_tables:
                                    debug_f.write(f'Tables: {sql_tables}\n')
                        except Exception as e:
                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Error harvesting DuckDB metadata: {str(e)}\n')
                                import traceback
                                debug_f.write(f'Traceback: {traceback.format_exc()}\n')

                    # Check for Spark session
                    elif obj_type == 'SparkSession':
                        try:
                            # Get tables from Spark catalog
                            tables = obj.catalog.listTables()
                            for table in tables:
                                table_name = table.name
                                sql_tables.append(table_name)

                                # Get columns for this table
                                try:
                                    columns = obj.catalog.listColumns(table_name)
                                    for col in columns:
                                        # Add fully qualified name (table.column)
                                        full_name = f"{table_name}.{col.name}"
                                        if full_name not in sql_columns:
                                            sql_columns.append(full_name)
                                        # Also add unqualified name (just column)
                                        if col.name not in sql_columns:
                                            sql_columns.append(col.name)
                                except:
                                    pass

                            # Get functions
                            try:
                                functions = obj.catalog.listFunctions()
                                for func in functions:
                                    sql_functions.append(func.name)
                            except:
                                pass

                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Spark SQL metadata: {len(sql_tables)} tables, {len(sql_columns)} columns, {len(sql_functions)} functions\n')
                        except Exception as e:
                            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                                debug_f.write(f'Error harvesting Spark metadata: {str(e)}\n')
                except:
                    pass

            with open('/tmp/sage_python_debug.txt', 'a') as debug_f:
                debug_f.write('=== INTROSPECTION END ===\n\n')

            # Send completions
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "completions", "data": completions}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)

            # Send type relationships
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "type_relationships", "data": {
                "return_types": return_types,
                "type_methods": type_methods
            }}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)

            # Send SQL metadata
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "sql_metadata", "data": {
                "tables": sql_tables,
                "columns": sql_columns,
                "functions": sql_functions
            }}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
        except Exception as e:
            # If completion gathering fails, don't crash - just send empty completions
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "completions", "data": []}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "type_relationships", "data": {
                "return_types": {},
                "type_methods": {}
            }}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "sql_metadata", "data": {
                "tables": [],
                "columns": [],
                "functions": []
            }}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)

        # Send result (only if not None, matching Jupyter behavior)
        if _sage_result is not None:
            # Format result in a Jupyter-like way
            try:
                # Import pprint for better formatting
                import pprint

                # Use a more intelligent formatting strategy
                if isinstance(_sage_result, str):
                    # For strings, use repr to show quotes
                    formatted = repr(_sage_result)
                elif isinstance(_sage_result, (list, dict, tuple, set)):
                    # For collections, use pprint for nice formatting
                    formatted = pprint.pformat(_sage_result, width=80, compact=True)
                else:
                    # For other types, try repr first, fallback to str
                    formatted = repr(_sage_result)
            except Exception:
                # If formatting fails, use str as last resort
                formatted = str(_sage_result)

            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "result", "data": formatted}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
        else:
            # No result to show (None result) - just signal success
            print("SAGE_OUTPUT_START", flush=True)
            print(json.dumps({"type": "success"}), flush=True)
            print("SAGE_OUTPUT_END", flush=True)
    except Exception as e:
        print("SAGE_OUTPUT_START", flush=True)
        error_data = {
            "type": "error",
            "ename": type(e).__name__,
            "evalue": str(e),
            "traceback": traceback.format_exc().split('\n')
        }
        print(json.dumps(error_data), flush=True)
        print("SAGE_OUTPUT_END", flush=True)
    except EOFError:
        break
    except Exception as e:
        print(f"REPL Error: {e}", file=sys.stderr, flush=True)
        break
"#
    }
}

impl Kernel for DirectKernel {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if self.is_connected() {
            return Ok(());
        }

        // Start Python process with our REPL script
        // Set TERM to dumb to avoid escape codes, and clear terminal-related env vars
        let mut child = Command::new(&self.info.python_path)
            .arg("-u") // Unbuffered output
            .arg("-c")
            .arg(Self::get_repl_script())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())  // Ignore stderr to avoid broken pipe
            .env("TERM", "dumb")  // Prevent terminal control codes
            .env_remove("TERM_PROGRAM")  // Remove any terminal program settings
            .spawn()
            .map_err(|e| format!("Failed to spawn Python process: {}", e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;

        // Wait for ready signal with timeout
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        // Try to read the ready signal
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF - process probably died
                return Err("Python process died immediately".into());
            }
            Ok(_) => {
                if !line.trim().starts_with("SAGE_KERNEL_READY") {
                    // Got unexpected output
                    return Err(format!(
                        "Kernel failed to start. Got: '{}'",
                        line.trim()
                    ).into());
                }
            }
            Err(e) => {
                return Err(format!("Failed to read from Python: {}", e).into());
            }
        }

        // Store process handle, stdin, and stdout reader
        self.stdin = Some(stdin);
        self.stdout = Some(reader);
        self.process = Some(child);

        Ok(())
    }

    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>> {
        if !self.is_connected() {
            return Err("Kernel not connected".into());
        }

        self.execution_count += 1;

        let stdin = self.stdin.as_mut().ok_or("No stdin available")?;
        let reader = self.stdout.as_mut().ok_or("No stdout available")?;

        // Send execution delimiters and code
        writeln!(stdin, "SAGE_EXEC_START")?;
        for line in code.lines() {
            writeln!(stdin, "{}", line)?;
        }
        writeln!(stdin, "SAGE_EXEC_END")?;
        stdin.flush()?;

        // Read outputs - there can be multiple output blocks (stdout, result, etc)
        let mut outputs = Vec::new();
        let mut completions = Vec::new();
        let mut type_relationships = crate::kernel::TypeRelationships::default();
        let mut sql_metadata = crate::kernel::SqlMetadata::default();
        let mut success = false;
        let mut finished = false;
        let mut line = String::new();

        while !finished {
            // Wait for output start marker
            loop {
                line.clear();
                reader.read_line(&mut line)?;
                if line.trim() == "SAGE_OUTPUT_START" {
                    break;
                }
            }

            // Read JSON output
            line.clear();
            reader.read_line(&mut line)?;

            let output_data: serde_json::Value = serde_json::from_str(line.trim())?;

            match output_data["type"].as_str() {
                Some("stdout") => {
                    if let Some(data) = output_data["data"].as_str() {
                        outputs.push(ExecutionOutput::Stdout(data.to_string()));
                    }
                }
                Some("result") => {
                    if let Some(data) = output_data["data"].as_str() {
                        outputs.push(ExecutionOutput::Result(data.to_string()));
                    }
                    success = true;
                    finished = true;
                }
                Some("success") => {
                    success = true;
                    finished = true;
                }
                Some("error") => {
                    let ename = output_data["ename"].as_str().unwrap_or("Error").to_string();
                    let evalue = output_data["evalue"].as_str().unwrap_or("").to_string();
                    let traceback = output_data["traceback"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    outputs.push(ExecutionOutput::Error {
                        ename,
                        evalue,
                        traceback,
                    });
                    success = false;
                    finished = true;
                }
                Some("completions") => {
                    // Parse completions for autocomplete
                    if let Some(data) = output_data["data"].as_array() {
                        for item in data {
                            if let Ok(completion) = serde_json::from_value::<crate::kernel::CompletionItem>(item.clone()) {
                                completions.push(completion);
                            }
                        }
                    }
                    // Don't set finished - continue reading for success/result markers
                }
                Some("type_relationships") => {
                    // Parse type relationship data for intelligent autocomplete
                    if let Some(data) = output_data.get("data") {
                        if let Ok(type_rel) = serde_json::from_value::<crate::kernel::TypeRelationships>(data.clone()) {
                            type_relationships = type_rel;
                        }
                    }
                    // Don't set finished - continue reading for success/result markers
                }
                Some("sql_metadata") => {
                    // Parse SQL metadata for SQL autocomplete
                    if let Some(data) = output_data.get("data") {
                        if let Ok(sql_meta) = serde_json::from_value::<crate::kernel::SqlMetadata>(data.clone()) {
                            sql_metadata = sql_meta;
                        }
                    }
                    // Don't set finished - continue reading for success/result markers
                }
                _ => {
                    finished = true;
                }
            }

            // Wait for output end marker
            line.clear();
            reader.read_line(&mut line)?;
        }

        Ok(ExecutionResult {
            outputs,
            execution_count: Some(self.execution_count),
            success,
            completions,
            type_relationships,
            sql_metadata,
        })
    }

    fn disconnect(&mut self) -> Result<(), Box<dyn Error>> {
        // Drop stdin first to send EOF to the Python process
        self.stdin = None;
        self.stdout = None;

        if let Some(mut process) = self.process.take() {
            // Try a quick check if it exited
            if let Ok(Some(_)) = process.try_wait() {
                return Ok(()); // Already exited
            }

            // Otherwise kill it immediately (the EOF from closing stdin should have signaled it)
            let _ = process.kill();
            let _ = process.wait();
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.process.is_some()
    }

    fn info(&self) -> KernelInfo {
        self.info.clone()
    }
}

impl Drop for DirectKernel {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}
