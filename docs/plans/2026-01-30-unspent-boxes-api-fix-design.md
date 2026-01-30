# Fix unspent_boxes_by_ergo_tree Return Type

**Date:** 2026-01-30
**Status:** Approved

## Problem

`unspent_boxes_by_ergo_tree` returns `Paged<ErgoBox>` and expects `{items, total}` JSON format, but Ergo node unspent endpoints return raw arrays without total counts.

| Function | Endpoint | Current Return | Works? |
|----------|----------|----------------|--------|
| `unspent_boxes_by_address` | unspent/byAddress | `Vec<ErgoBox>` | Yes |
| `unspent_boxes_by_token_id` | unspent/byTokenId | `Vec<ErgoBox>` | Yes |
| `unspent_boxes_by_ergo_tree` | unspent/byErgoTree | `Paged<ErgoBox>` | No |

## Solution

Change `unspent_boxes_by_ergo_tree` to return `Vec<ErgoBox>` for consistency with other unspent functions.

## API Changes

**Before:**
```rust
pub async fn unspent_boxes_by_ergo_tree(&self, ergo_tree: &str, offset: u64, limit: u64)
    -> Result<Paged<ErgoBox>>
```

**After:**
```rust
pub async fn unspent_boxes_by_ergo_tree(&self, ergo_tree: &str, offset: u64, limit: u64)
    -> Result<Vec<ErgoBox>>
```

**Breaking change:** Callers using `.items` and `.total` will get compile errors. Fix by using the `Vec` directly.

## Implementation

Replace the current implementation with the array-iteration pattern used by the other unspent functions:

```rust
pub async fn unspent_boxes_by_ergo_tree(
    &self,
    ergo_tree: &str,
    offset: u64,
    limit: u64,
) -> Result<Vec<ErgoBox>> {
    self.require_extra_index()?;
    let endpoint = format!(
        "/blockchain/box/unspent/byErgoTree?offset={}&limit={}",
        offset, limit
    );
    let res = self.send_post_req(&endpoint, ergo_tree.to_string()).await;
    let res_json = self.parse_response_to_json(res).await?;

    let mut box_list = vec![];
    for i in 0.. {
        let box_json = &res_json[i];
        if box_json.is_null() {
            break;
        } else if let Ok(ergo_box) = from_str(&box_json.to_string()) {
            if box_json["spentTransactionId"].is_null() {
                box_list.push(ergo_box);
            }
        }
    }
    Ok(box_list)
}
```

## Test Updates

1. `test_paged_unspent_boxes_by_ergo_tree` → rename to `test_unspent_boxes_by_ergo_tree`
   - Mock raw array response instead of `{items, total}`
   - Assert on `result.len()` instead of `result.items.len()` and `result.total`

2. `test_unspent_boxes_by_ergo_tree_404_returns_empty`
   - Expect empty `Vec` instead of empty `Paged`

3. `test_unspent_boxes_by_ergo_tree_filters_spent_boxes`
   - Mock raw array format
   - Verify filtering works on `Vec` result

## Documentation Updates

Add to all three unspent functions:

```rust
/// Note: The Ergo node's unspent endpoints do not provide a total count.
/// Because spent boxes are filtered client-side, the returned count may be
/// less than `limit` even when more boxes exist. Pagination should continue
/// until an empty result is returned.
```

## Review Feedback (Post-Implementation)

1. **404 handling restored**: Added `handle_paged_404` check before JSON parsing to properly handle 404 responses (return empty Vec when extraIndex is enabled).

2. **Pagination guidance corrected**: Updated docs to clarify that pagination should continue until empty result, not until `len() < limit`, because client-side spent box filtering can reduce the count.

## Files to Modify

- `src/node_interface.rs`: Function implementation and doc comments
- `src/node_interface.rs` (tests section): Update three test functions
