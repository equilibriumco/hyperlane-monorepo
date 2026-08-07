use aiken/primitive/bytearray
use cardano/assets.{from_asset, from_lovelace, merge}
use utils

/// Test hyperlane_address_to_script_hash with valid 32-byte address
test hyperlane_address_to_script_hash_valid() {
  // 4-byte prefix + 28-byte hash = 32 bytes total
  let prefix = #"02000000"
  let hash = #"0123456789abcdef0123456789abcdef0123456789abcdef01234567"
  let addr = bytearray.concat(prefix, hash)

  when utils.hyperlane_address_to_script_hash(addr) is {
    Some(result) -> result == hash
    None -> False
  }
}

/// Test hyperlane_address_to_script_hash with invalid length
test hyperlane_address_to_script_hash_invalid_length() {
  let short_addr = #"0123456789abcdef"

  when utils.hyperlane_address_to_script_hash(short_addr) is {
    Some(_) -> False
    None -> True
  }
}

/// Test script_hash_to_hyperlane_address roundtrip
test script_hash_to_hyperlane_roundtrip() {
  let hash = #"0123456789abcdef0123456789abcdef0123456789abcdef01234567"
  let hyperlane_addr = utils.script_hash_to_hyperlane_address(hash)

  when utils.hyperlane_address_to_script_hash(hyperlane_addr) is {
    Some(result) -> result == hash
    None -> False
  }
}

/// Test script_hash_to_hyperlane_address produces correct prefix
test script_hash_to_hyperlane_has_correct_prefix() {
  let hash = #"0123456789abcdef0123456789abcdef0123456789abcdef01234567"
  let hyperlane_addr = utils.script_hash_to_hyperlane_address(hash)

  // Check that the first 4 bytes are the script credential prefix
  // bytearray.slice uses inclusive end index, so 0-3 = 4 bytes
  bytearray.slice(hyperlane_addr, 0, 3) == #"02000000"
}

/// Test at function with valid index
test at_valid_index() {
  let items = [1, 2, 3, 4, 5]

  when utils.at(items, 2) is {
    Some(value) -> value == 3
    None -> False
  }
}

/// Test at function with index 0
test at_first_element() {
  let items = [10, 20, 30]

  when utils.at(items, 0) is {
    Some(value) -> value == 10
    None -> False
  }
}

/// Test at function with last valid index
test at_last_element() {
  let items = [10, 20, 30]

  when utils.at(items, 2) is {
    Some(value) -> value == 30
    None -> False
  }
}

/// Test at function with out of bounds index
test at_out_of_bounds() {
  let items = [1, 2, 3]

  when utils.at(items, 5) is {
    Some(_) -> False
    None -> True
  }
}

/// Test at function with empty list
test at_empty_list() {
  let items: List<Int> = []

  when utils.at(items, 0) is {
    Some(_) -> False
    None -> True
  }
}

/// Test at function with negative index (should return None)
test at_negative_index() {
  let items = [1, 2, 3]

  when utils.at(items, -1) is {
    Some(_) -> False
    None -> True
  }
}

/// Test assoc_find with existing key
test assoc_find_existing_key() {
  let items = [(1, "one"), (2, "two"), (3, "three")]

  when utils.assoc_find(items, 2) is {
    Some(value) -> value == "two"
    None -> False
  }
}

/// Test assoc_find with first key
test assoc_find_first_key() {
  let items = [(1, "one"), (2, "two"), (3, "three")]

  when utils.assoc_find(items, 1) is {
    Some(value) -> value == "one"
    None -> False
  }
}

/// Test assoc_find with last key
test assoc_find_last_key() {
  let items = [(1, "one"), (2, "two"), (3, "three")]

  when utils.assoc_find(items, 3) is {
    Some(value) -> value == "three"
    None -> False
  }
}

/// Test assoc_find with non-existent key
test assoc_find_non_existent_key() {
  let items = [(1, "one"), (2, "two"), (3, "three")]

  when utils.assoc_find(items, 4) is {
    Some(_) -> False
    None -> True
  }
}

/// Test assoc_find with empty list
test assoc_find_empty_list() {
  let items: List<(Int, String)> = []

  when utils.assoc_find(items, 1) is {
    Some(_) -> False
    None -> True
  }
}

/// Test has_at_least with sufficient elements
test has_at_least_sufficient() {
  let items = [1, 2, 3, 4, 5]
  utils.has_at_least(items, 3)
}

/// Test has_at_least with exact count
test has_at_least_exact() {
  let items = [1, 2, 3]
  utils.has_at_least(items, 3)
}

/// Test has_at_least with insufficient elements
test has_at_least_insufficient() {
  let items = [1, 2]
  !utils.has_at_least(items, 3)
}

/// Test has_at_least with zero requirement
test has_at_least_zero() {
  let items = [1]
  utils.has_at_least(items, 0)
}

/// Test has_at_least empty list with zero requirement
test has_at_least_empty_zero() {
  let items: List<Int> = []
  utils.has_at_least(items, 0)
}

// ============================================================================
// value_preserved tests
// ============================================================================

const test_nft_policy = #"f1bd8bb9bbd7d3fe0a20b12e1d8f36ab717c6e9c8e796f5375400378"

const test_token_policy = #"abcdef0123456789abcdef0123456789abcdef0123456789abcdef01"

test value_preserved_lovelace_and_nft() {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  utils.value_preserved(input, output)
}

test value_preserved_extra_lovelace_ok() {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = merge(from_lovelace(10_000_000), from_asset(test_nft_policy, "", 1))
  utils.value_preserved(input, output)
}

test value_preserved_fails_when_nft_missing() fail {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = from_lovelace(5_000_000)
  utils.value_preserved(input, output)
}

test value_preserved_fails_when_lovelace_reduced() fail {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = merge(from_lovelace(1_000_000), from_asset(test_nft_policy, "", 1))
  utils.value_preserved(input, output)
}

test value_preserved_fails_when_nft_stolen_lovelace_ok() fail {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = from_lovelace(5_000_000)
  utils.value_preserved(input, output)
}

test value_preserved_multiple_tokens() {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  utils.value_preserved(input, output)
}

test value_preserved_fails_when_one_token_missing() fail {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
  utils.value_preserved(input, output)
}

test value_preserved_lovelace_only() {
  let input = from_lovelace(5_000_000)
  let output = from_lovelace(5_000_000)
  utils.value_preserved(input, output)
}

// ============================================================================
// native_tokens_preserved tests
// ============================================================================

test native_tokens_preserved_ignores_lovelace_decrease() {
  let input = merge(from_lovelace(10_000_000), from_asset(test_nft_policy, "", 1))
  let output = merge(from_lovelace(3_000_000), from_asset(test_nft_policy, "", 1))
  utils.native_tokens_preserved(input, output)
}

test native_tokens_preserved_fails_when_nft_missing() fail {
  let input = merge(from_lovelace(5_000_000), from_asset(test_nft_policy, "", 1))
  let output = from_lovelace(5_000_000)
  utils.native_tokens_preserved(input, output)
}

test native_tokens_preserved_token_increase_ok() {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 50))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  utils.native_tokens_preserved(input, output)
}

test native_tokens_preserved_fails_when_token_decreased() fail {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 50))
  utils.native_tokens_preserved(input, output)
}

// ============================================================================
// native_tokens_preserved_except tests
// ============================================================================

test native_tokens_preserved_except_allows_excepted_token_decrease() {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 50))
  utils.native_tokens_preserved_except(input, output, test_token_policy, "TokenA")
}

test native_tokens_preserved_except_allows_excepted_token_removal() {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
  utils.native_tokens_preserved_except(input, output, test_token_policy, "TokenA")
}

test native_tokens_preserved_except_still_protects_nft() fail {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_token_policy, "TokenA", 50))
  utils.native_tokens_preserved_except(input, output, test_token_policy, "TokenA")
}

test native_tokens_preserved_except_wrong_name_not_excluded() fail {
  let input =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 100))
  let output =
    from_lovelace(5_000_000)
      |> merge(from_asset(test_nft_policy, "", 1))
      |> merge(from_asset(test_token_policy, "TokenA", 50))
  // Excluding "TokenB" should NOT exempt "TokenA"
  utils.native_tokens_preserved_except(input, output, test_token_policy, "TokenB")
}
