use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    program_error::ProgramError,
    pubkey::Pubkey,
};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() < 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let tx_id = &instruction_data[..32];

    let account_info_iter = &mut accounts.iter();
    let signer = next_account_info(account_info_iter)?;
    let pda_account = next_account_info(account_info_iter)?;

    let (expected_pda, _bump) =
        Pubkey::find_program_address(&[b"unique", signer.key.as_ref(), tx_id], program_id);

    if expected_pda != *pda_account.key {
        return Err(ProgramError::InvalidArgument);
    }

    let data = pda_account.try_borrow_data()?;
    if data[0] != 0 {
        return Err(ProgramError::Custom(0)); // Duplicate detected
    }
    drop(data);

    let mut data = pda_account.try_borrow_mut_data()?;
    data[0] = 1;

    Ok(())
}
