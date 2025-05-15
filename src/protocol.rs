use log::{debug, info};

const NO_MAGIC: [u8; 2] = [0x2c, 0x08];

pub const SUCCESS: usize = 0;
pub const FAIL: usize = 1;

const CHUNK_SIZE: usize = 256;

const EFUSE_BASE: u64 = 0x0E00_0000;
const BOOT_SRC_ADDR: u64 = EFUSE_BASE + 0x0004;
// plat/cv180x/include/platform_def.h
// plat/cv181x/include/platform_def.h
const BOOT_SRC_USB: [u8; 4] = *b"1NGM";
const BOOT_SRC_SD: [u8; 4] = *b"2NGM";
// CV181x only?
const BOOT_SRC_UART: [u8; 4] = *b"3NGM";

// Memory addresses
pub const DDR_FIP_ADDR: u64 = 0x8080_0000;
pub const IMG_ADDR: u64 = 0x8394_0000;

// ROM USB command;
pub const CVI_USB_TX_DATA_TO_RAM: u8 = 0;
pub const CVI_USB_TX_FLAG: u8 = 1;
// Common command;
pub const CV_USB_BREAK: u8 = 2;
pub const CV_USB_KEEP_DL: u8 = 3;
pub const CV_USB_UBREAK: u8 = 4;
pub const CV_USB_PRG_CMD: u8 = 6;
pub const CVI_USB_REBOOT: u8 = 22;
pub const CVI_USB_PROGRAM: u8 = 0x83;

pub const MSG_TOKEN_OFFSET: usize = 0;

type Port = std::boxed::Box<dyn serialport::SerialPort>;

#[derive(Debug)]
struct Header {
    cmd: u8,
    size: u16,
    addr: u64,
}

const HEADER_SIZE: usize = 8;

impl Header {
    fn to_slice(&self) -> [u8; HEADER_SIZE] {
        // add header's own size
        let sz = self.size + HEADER_SIZE as u16;
        let l1 = (sz >> 8) as u8;
        let l0 = sz as u8;
        let a4 = (self.addr >> 32) as u8;
        let a3 = (self.addr >> 24) as u8;
        let a2 = (self.addr >> 16) as u8;
        let a1 = (self.addr >> 8) as u8;
        let a0 = self.addr as u8;
        [self.cmd, l1, l0, a4, a3, a2, a1, a0]
    }
}

const NO_MAGIC_HEADER: Header = Header {
    cmd: CV_USB_KEEP_DL,
    size: NO_MAGIC.len() as u16,
    addr: 0,
};

const BOOT_SRC_USB_HEADER: Header = Header {
    cmd: CVI_USB_TX_FLAG,
    size: BOOT_SRC_USB.len() as u16,
    addr: BOOT_SRC_ADDR,
};

const BREAK_HEADER: Header = Header {
    cmd: CV_USB_BREAK,
    size: 0,
    addr: 0,
};

const RESPONSE_SIZE: usize = 16;

// CRC variant figured out by trying all the CRC16s offered by crc crate :D
const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_XMODEM);

// TODO: struct
const RSP_CRC16_HI_OFFSET: usize = 2;
const RSP_CRC16_LO_OFFSET: usize = 3;
const RSP_TOKEN_OFFSET: usize = 6;

fn check_response(data: &[u8], resp: &[u8]) {
    info!("response: {resp:x?}");

    let crc_bytes = [resp[RSP_CRC16_LO_OFFSET], resp[RSP_CRC16_HI_OFFSET]];
    let rsp_checksum = u16::from_le_bytes(crc_bytes);
    let exp_checksum = CRC.checksum(data);

    if exp_checksum != rsp_checksum {
        panic!("Checksum mismatch: got {rsp_checksum:04x}, expected {exp_checksum:04x}");
    }

    info!("checksum {rsp_checksum:04x} == {exp_checksum:04x}");

    let rsp_token = resp[RSP_TOKEN_OFFSET];
    info!("token: {rsp_token:02x}");
}

fn send(port: &mut Port, data: &[u8]) {
    debug!("data: {data:x?}");
    let sent = port.write(data).expect("Write failed!");
    let mut resp: Vec<u8> = vec![0; RESPONSE_SIZE];
    let read = port.read(resp.as_mut_slice()).expect("Found no data!");
    check_response(data, &resp);
    info!("sent {sent} bytes, read {read} bytes");
}

pub fn concat(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().chain(b).copied().collect()
}

pub fn send_magic(port: &mut Port) {
    info!("send NO MAGIC...");
    debug!("{NO_MAGIC_HEADER:?}");
    let data = concat(&NO_MAGIC_HEADER.to_slice(), NO_MAGIC.as_slice());
    send(port, &data);
}

pub fn send_file(port: &mut Port, f: &[u8]) {
    for (i, chunk) in f.chunks(CHUNK_SIZE).enumerate() {
        let h = Header {
            cmd: CVI_USB_TX_DATA_TO_RAM,
            size: chunk.len() as u16,
            addr: (i * CHUNK_SIZE) as u64,
        };
        debug!("{h:?}");
        let data = concat(&h.to_slice(), chunk);
        send(port, &data);
    }
}

pub fn send_flag(port: &mut Port) {
    let data = concat(&BOOT_SRC_USB_HEADER.to_slice(), &BOOT_SRC_USB);
    send(port, &data);
}

pub fn send_break(port: &mut Port) {
    let data = BREAK_HEADER.to_slice();
    send(port, &data);
}

pub const IMG_ALIGN: usize = 512;

fn zeroes<const N: usize>() -> [u8; N] {
    [0u8; N]
}

fn ones<const N: usize>() -> [u8; N] {
    [0xffu8; N]
}

#[derive(Debug)]
#[repr(C)]
pub struct CVITekHeader {
    pub magic: [u8; 8],
    pub _pad: u32,
    pub param1_checksum: [u8; 4],
    pub param1: Param1,
}

const HEADER_MAGIC: [u8; 8] = *b"CVBL01\n\0";
impl Default for CVITekHeader {
    fn default() -> Self {
        CVITekHeader {
            magic: HEADER_MAGIC,
            _pad: 0,
            param1_checksum: zeroes(),
            param1: Param1::default(),
        }
    }
}

impl CVITekHeader {
    pub fn to_slice(&self) -> &[u8] {
        let size = 4096 - 16;
        let ptr = self as *const Self as *const u8;
        unsafe { std::slice::from_raw_parts(ptr, size) }
    }
}

// see plat/cv181x/include/platform_def.h
struct SpiNandInfo {
    //       uint32_t version;
    //       uint32_t id;
    //       uint32_t page_size;
    //       uint32_t spare_size;
    //       uint32_t block_size;
    //       uint32_t pages_per_block;
    //       uint32_t fip_block_cnt;
    //       uint8_t pages_per_block_shift;
    //       uint8_t badblock_pos;
    //       uint8_t dummy_data1[2];
    //       uint32_t flags;
    //       uint8_t ecc_en_feature_offset;
    //       uint8_t ecc_en_mask;
    //       uint8_t ecc_status_offset;
    //       uint8_t ecc_status_mask;
    //       uint8_t ecc_status_shift;
    //       uint8_t ecc_status_uncorr_val;
    //       uint8_t dummy_data2[2];
    //       uint32_t erase_count; // erase count for sys base block
    //       uint8_t sck_l;
    //       uint8_t sck_h;
    //       uint16_t max_freq;
    //       uint32_t sample_param;
    //       uint8_t xtal_switch;
    //       uint8_t dummy_data3[71];
}

struct SpiNorInfo {
    //      uint32_t ctrl;
    //      uint32_t dly_ctrl;
    //      uint32_t tran_csr;
    //      uint32_t opt;
    //      uint32_t reserved_1;
    //      uint32_t reserved_2;
    //      uint32_t reserved_3;
    //      uint32_t reserved_4;
    //      uint32_t reserved_5;
}

/* ....... */

#[derive(Debug)]
#[repr(C)]
pub struct Param1 {
    pub nand_info: [u8; 128],
    pub nor_info: [u8; 36],
    pub fip_flags: [u8; 8],
    pub chip_conf_size: [u8; 4], // 760, matching up with chip_conf+_pad below
    // BLCP
    pub blcp_img_cksum: [u8; 4],
    pub blcp_img_size: [u8; 4],
    pub blcp_img_runaddr: [u8; 4],
    pub blcp_param_loadaddr: [u8; 4],
    pub blcp_param_size: [u8; 4],
    // BL2
    pub bl2_img_cksum: [u8; 4],
    pub bl2_img_size: [u8; 4],
    //
    pub bld_img_size: [u8; 4],
    pub param2_loadaddr: [u8; 4],
    pub reserved1: [u8; 4],
    pub chip_conf: [u8; 24], // originally 760
    pub _pad: [u8; 736],
    pub bl_ek: [u8; 32],
    // ...
    pub root_pk: [u8; 512],
    pub bl_pk: [u8; 512],
    // last 2k
    pub bl_pk_sig: [u8; 512],
    pub chip_conf_sig: [u8; 512],
    pub bl2_img_sig: [u8; 512],
    pub blcp_img_sig: [u8; 512],
}

const CONF_SIZE: u32 = 0x0000_02f8;
const NO_CHECKSUM: u32 = 0xcafe_0000;
const BLCP_ADDR: u32 = 0x0520_0200;
const PARAM2_LOAD_ADDR: u32 = 0x0000_2a00;

impl Default for Param1 {
    fn default() -> Self {
        Param1 {
            nand_info: zeroes(),
            nor_info: ones(),
            fip_flags: zeroes(),
            // ...
            chip_conf_size: CONF_SIZE.to_le_bytes(),
            // BLCP
            blcp_img_cksum: NO_CHECKSUM.to_le_bytes(),
            blcp_img_size: zeroes(),
            blcp_img_runaddr: BLCP_ADDR.to_le_bytes(),
            blcp_param_loadaddr: zeroes(),
            blcp_param_size: zeroes(),
            // BL2
            bl2_img_cksum: NO_CHECKSUM.to_le_bytes(),
            bl2_img_size: zeroes(),
            //
            bld_img_size: zeroes(),
            param2_loadaddr: PARAM2_LOAD_ADDR.to_le_bytes(),
            reserved1: zeroes(),
            chip_conf: [
                0x0c, 0x00, 0x00, 0x0e, //
                0x01, 0x00, 0x00, 0xa0, //
                0x0c, 0x00, 0x00, 0x0e, //
                0x02, 0x00, 0x00, 0xa0, //
                0xa0, 0xff, 0xff, 0xff, //
                0xff, 0xff, 0xff, 0xff, //
            ], // ??
            // after 24 bytes, just zeroes
            _pad: zeroes(),
            // keys and signatures
            bl_ek: zeroes(),
            root_pk: zeroes(),
            bl_pk: zeroes(),
            bl_pk_sig: zeroes(),
            chip_conf_sig: zeroes(),
            bl2_img_sig: zeroes(),
            blcp_img_sig: zeroes(),
        }
    }
}

impl Param1 {
    pub fn checksum(&self) -> u16 {
        let h1_size = 2048 - 16;
        let h1_ptr = self as *const Self as *const u8;
        let h1 = unsafe { std::slice::from_raw_parts(h1_ptr, h1_size) };
        CRC.checksum(h1)
    }

    pub fn to_slice(&self) -> &[u8] {
        let size = 4096 - 16;
        let ptr = self as *const Self as *const u8;
        unsafe { std::slice::from_raw_parts(ptr, size) }
    }
}
