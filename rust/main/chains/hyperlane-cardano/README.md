### Deploy local Cardano validator
In a separate terminal, cd to `hyperlane-monorepo/rust`

#### 1. Source the env vars:
```shell
source ./config/cardano/validator.env
```

#### 2. Run the validator (the `rm` is to make sure the validator's DB is cleared):
```shell
rm -rf /tmp/cardanoDb/validator 
CONFIG_FILES=./config/cardano/cardano.json cargo run --bin validator
```