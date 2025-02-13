## How to use
move `.env.default` to `.env` and fill. then,

`cargo run --bin cli`

click "copy transaction" in phantom in a transaction. eg from pump.fun:

```sh
curl -X POST http://localhost:3000/submit_base64 \
-H "Content-Type: application/json" \
-d '{
    "base64_tx": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAQAJDu419ykC6U2MZIRQ3FAuAz6OnBTtoZ2BoE+AQXQ2Deh10Msvaz37yZwluzFI8b0LxnoH48WlK2Gj3LpXs3v4h62tEeak/ClEpPqCUb74FUJuG/soxrZkZndgfGrZ9WamRmW5uzWA6bCCweASd2ostkkSASq7Abucvqhdu+LmygXPTnjK5gXLADdGnivl6s7pr4U6AIXSuamx89x+PNM8ZrgDBkZv5SEXMv/srbpyw5vnvIzlu8X3EmssQ5s6QAAAAIyXJY9OJInxuz0QKRSODYMLWhOZ2v8QhASOe9jb6fhZ/jGHw0YcbuU6nNdX04HwsZ+JQFX8Bb0fG8j+5MpGY+8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAbd9uHXZaGT2cvhRs7reawctIXtX1s3kTqM9YV+/wCpAVbg9pNmWs9E2xVovxdbqlGJy5f10v87ZV0rtv1tGLA6hl5p7g9UgMq89mNX5NwvGNWNRcHqdIn7NyPZeTxypgan1RcZLFxRIYzJTD1K8X9Y2u4Im6H9ROPb2YoAAAAArPE26wH8HE6IPSPItYRKtZo39mrdV8XprDtT4FnTXGSvcnwNQBan6Ix+Eof8SeyESuaXR1g8V0vsXQjwKzlwDQQFAAUCS/IAAAUACQNonSkAAAAAAAYGAAEABwgJAQEKDAsCBwMEAQAICQwNChhmBj0SAdrr6kkbAegBAAAAOLgPAAAAAAAA"
}'
```

## Writeup
first, an overview of the requirements:
- Construct Unique Transactions
  - We do this via different blockhashes and priority fees. We get priority fees for each individual txn via helius which returns different priority fees, as well as inserting one of the last 10 blockhashes (which we fetch n every sec where n is the number of `RPC_URLS`, so well within the limit.)
- Parallel Transaction Submission Across Multiple RPC Nodes
  - We submit in order of latency from `RPC_URLS`, via our rpc pool.
- Implement Transaction Retrying with Dynamic Adaptation
  - Requires "Track and Log Execution Performance", ran out of time :(
- Latency Profiling & Optimized RPC Selection
  - Done via RPC pool; no logging, goes with "Track and Log Execution Performance"
- Handle Jito and Non-Jito Validators
  - Ran out of time. If I implemented this, I'd exclude the priority fee and add a tip transaction to a bundle, then send it to a jito validator if the next block's leader was a jito validator.
- Monitor and Cancel Redundant Transactions
  - We have a smart contract which flips a bit for every given transactions such that only one transaction can land, then the rest would cheaply revert. Another way I was thinking about doing this was to use extremely old nonces such that they were only valid for a couple blocks. Then, I'd submit them, wait for the blockhash to expire, then submit them to the next rpc.
- Track and Log Execution Performance
  - Ran out of time, but I would have created a postgres db with tables for origination tx hash, submitted tx hashes, tx first seen, tx landed, tx not landed reason, etc.
