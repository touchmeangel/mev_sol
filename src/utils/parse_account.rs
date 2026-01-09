use bytemuck::Pod;

pub fn parse_account<T: Pod>(
  data: &[u8],
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
  let marginfi_account = bytemuck::try_from_bytes::<T>(&data[8..])
      .map_err(|e| format!("account data parse failed: {:?}", e))?;
  
  Ok(*marginfi_account)
}