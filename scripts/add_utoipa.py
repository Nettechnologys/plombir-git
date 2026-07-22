#!/usr/bin/env python3
"""Auto-generate utoipa::path annotations for IronForge API handlers.

Reads route definitions from lib.rs, maps handler names to (method, path),
then adds utoipa::path annotations to handlers that don't have them.
"""
import re
import os
import sys

BASE = "/Users/yuqu/Vbercodeing/ironforge/crates/rg-http/src/api"

# Module → Tag mapping
MODULE_TAGS = {
    "users": "Users",
    "repos": "Repositories",
    "issues": "Issues",
    "labels": "Labels",
    "pulls": "Pull Requests",
    "reviews": "Reviews",
    "wiki": "Wiki",
    "lfs": "LFS",
    "webhooks": "Webhooks",
    "ci": "CI/CD",
    "releases": "Releases",
    "orgs": "Organizations",
    "notifications": "Notifications",
    "admin": "Admin",
    "search": "Search",
    "branch_protection": "Branch Protection",
    "collaborators": "Collaborators",
    "repo_content": "Repository Content",
    "runners": "Runners",
    "artifacts": "Artifacts",
}

# HTTP method to utoipa keyword
METHOD_MAP = {
    "get": "get",
    "post": "post",
    "put": "put",
    "patch": "patch",
    "delete": "delete",
}

def parse_routes(lib_rs_path):
    """Parse lib.rs to extract handler → (method, path) mapping."""
    with open(lib_rs_path) as f:
        content = f.read()

    handler_map = {}  # module_name::handler_name → (method, path)

    # Pattern 1: Simple routes: .route("/path", method(api::module::handler))
    simple = r'\.route\(\s*"([^"]+)"\s*,\s*(get|post|put|patch|delete)\(api::([^)]+)\)\s*\)'
    for path, method, handler_path in re.findall(simple, content):
        parts = handler_path.strip().split("::")
        # Use module::handler as key to avoid name collisions
        if len(parts) >= 2:
            handler_key = f"{parts[-2]}::{parts[-1]}"
        else:
            handler_key = parts[-1]
        handler_map[handler_key] = (method, path)

    # Pattern 2: Chained routes: .route("/path", get(api::m::h1).post(api::m::h2))
    chain_pattern = r'\.route\(\s*"([^"]+)"\s*,\s*((?:get|post|put|patch|delete)\(api::[^)]+\)(?:\.(?:get|post|put|patch|delete)\(api::[^)]+\))*)\s*\)'
    for path, chain in re.findall(chain_pattern, content):
        for m, h in re.findall(r'(get|post|put|patch|delete)\(api::([^)]+)\)', chain):
            parts = h.strip().split("::")
            if len(parts) >= 2:
                handler_key = f"{parts[-2]}::{parts[-1]}"
            else:
                handler_key = parts[-1]
            handler_map[handler_key] = (m, path)

    return handler_map


def extract_path_params(path):
    """Extract path parameters from a route path like /repos/{owner}/{name}."""
    return re.findall(r'\{(\w+)\}', path)


def extract_path_params_annot(path):
    """Generate utoipa params from path."""
    params = extract_path_params(path)
    if not params:
        return ""

    lines = []
    for p in params:
        # Try to infer type - use String for names, i64 for ids
        if p in ("id", "number", "job_id", "delivery_id", "asset_id", "release_id", "team_id", "user_id"):
            lines.append(f'        ("{p}" = i64, Path, description = "{p}"),')
        else:
            lines.append(f'        ("{p}" = String, Path, description = "{p}"),')
    return "\n".join(lines)


def has_utoipa_annotation(lines, fn_line_idx):
    """Check if the function at fn_line_idx already has a #[utoipa::path] annotation."""
    # Look back up to 25 lines for a #[utoipa::path annotation
    for j in range(max(0, fn_line_idx - 25), fn_line_idx):
        if "#[utoipa::path" in lines[j]:
            return True
    return False


def process_file(filepath, module_name, handler_map, tag):
    """Add utoipa::path annotations to a single API file."""
    with open(filepath) as f:
        content = f.read()

    lines = content.split("\n")
    new_lines = []
    changed = False

    i = 0
    while i < len(lines):
        line = lines[i]

        # Check if this is a pub async fn handler
        match = re.match(r'^(\s*)(pub\s+async\s+fn\s+)(\w+)', line)
        if match and "test" not in line.lower():
            indent = match.group(1)
            handler_name = match.group(3)
            handler_key = f"{module_name}::{handler_name}"

            if handler_key in handler_map and not has_utoipa_annotation(lines, i):
                method, path = handler_map[handler_key]
                params_annot = extract_path_params_annot(path)

                # Determine if it has a request body (POST/PUT/PATCH with Json extractor)
                has_body = method in ("post", "put", "patch")
                # Check next few lines for Json extractor
                if has_body:
                    # Look for Json<body> in the next 10 lines
                    fn_block = "\n".join(lines[i:i+10])
                    if "Json(" not in fn_block and "Bytes" not in fn_block:
                        has_body = False

                # Build the annotation
                annotation_lines = [f"{indent}#[utoipa::path("]
                annotation_lines.append(f'{indent}    {method},')

                # Use the path without /api/v1 prefix (utoipa uses relative paths)
                display_path = path
                if display_path.startswith("/api/v1"):
                    display_path = display_path[len("/api/v1"):]
                annotation_lines.append(f'{indent}    path = "{display_path}",')
                annotation_lines.append(f'{indent}    tag = "{tag}",')

                if params_annot:
                    annotation_lines.append(f'{indent}    params(')
                    annotation_lines.append(params_annot)
                    annotation_lines.append(f'{indent}    ),')

                if has_body:
                    # Find request body type
                    fn_block = "\n".join(lines[i:i+10])
                    body_match = re.search(r'Json<([^>]+)>', fn_block)
                    body_type = body_match.group(1) if body_match else "serde_json::Value"
                    annotation_lines.append(f'{indent}    request_body(content = serde_json::Value),')

                # Responses
                if method == "post":
                    annotation_lines.append(f'{indent}    responses(')
                    annotation_lines.append(f'{indent}        (status = 201, description = "Created", body = serde_json::Value),')
                    annotation_lines.append(f'{indent}        (status = 400, description = "Bad request", body = serde_json::Value),')
                    annotation_lines.append(f'{indent}        (status = 401, description = "Unauthorized", body = serde_json::Value),')
                elif method == "put" or method == "patch":
                    annotation_lines.append(f'{indent}    responses(')
                    annotation_lines.append(f'{indent}        (status = 200, description = "Updated", body = serde_json::Value),')
                    annotation_lines.append(f'{indent}        (status = 401, description = "Unauthorized", body = serde_json::Value),')
                elif method == "delete":
                    annotation_lines.append(f'{indent}    responses(')
                    annotation_lines.append(f'{indent}        (status = 200, description = "Deleted", body = serde_json::Value),')
                    annotation_lines.append(f'{indent}        (status = 204, description = "No content"),')
                    annotation_lines.append(f'{indent}        (status = 401, description = "Unauthorized", body = serde_json::Value),')
                else:  # get
                    annotation_lines.append(f'{indent}    responses(')
                    annotation_lines.append(f'{indent}        (status = 200, description = "Success", body = serde_json::Value),')
                    annotation_lines.append(f'{indent}        (status = 401, description = "Unauthorized", body = serde_json::Value),')

                annotation_lines.append(f'{indent}    ),')
                annotation_lines.append(f'{indent})]')

                # Insert annotation before the function
                for al in annotation_lines:
                    new_lines.append(al)
                changed = True

        new_lines.append(line)
        i += 1

    if changed:
        with open(filepath, "w") as f:
            f.write("\n".join(new_lines))
        print(f"  ✅ Updated {module_name}.rs ({len([l for l in new_lines if '#[utoipa::path' in l])} annotations)")
    else:
        print(f"  ⏭️  Skipped {module_name}.rs (already annotated)")

    return changed


def add_toschema_import(filepath):
    """Ensure 'use utoipa::ToSchema;' is present in the file."""
    with open(filepath) as f:
        content = f.read()

    if "use utoipa::ToSchema" in content:
        return False

    # Add import after the last 'use' statement, or after the module doc comment
    if "use utoipa" in content:
        # Already has some utoipa import
        content = content.replace("use utoipa::ToSchema;", "")
        content = content.replace("use utoipa;", "use utoipa::ToSchema;")
        with open(filepath, "w") as f:
            f.write(content)
        return True

    # Find the right place to insert
    lines = content.split("\n")
    insert_idx = 0
    for i, line in enumerate(lines):
        if line.startswith("use "):
            insert_idx = i + 1
        elif line.startswith("//!") or line.strip() == "":
            continue
        else:
            break

    lines.insert(insert_idx, "use utoipa::ToSchema;")
    with open(filepath, "w") as f:
        f.write("\n".join(lines))
    return True


def main():
    lib_rs = os.path.join(os.path.dirname(BASE), "lib.rs")
    print(f"🔄 Parsing routes from {lib_rs}...")
    handler_map = parse_routes(lib_rs)
    print(f"📋 Found {len(handler_map)} route handlers")

    # Process each module
    total_changed = 0
    for module_name, tag in MODULE_TAGS.items():
        filepath = os.path.join(BASE, f"{module_name}.rs")
        if not os.path.exists(filepath):
            print(f"  ⚠️  {module_name}.rs not found, skipping")
            continue

        add_toschema_import(filepath)
        if process_file(filepath, module_name, handler_map, tag):
            total_changed += 1

    print(f"\n✨ Done! Updated {total_changed} files.")
    print("⚠️  Run 'cargo build' to verify compilation.")


if __name__ == "__main__":
    main()
