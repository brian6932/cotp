use std::{fs::File, io::Write, path::PathBuf, vec};

use crate::{
    crypto::cryptography::{argon_derive_key, encrypt_string_with_key, gen_salt},
    utils,
};
use data_encoding::BASE32_NOPAD;
use qrcode::render::unicode;
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::{
    algorithms::{
        hotp_maker::hotp, motp_maker::motp, steam_otp_maker::steam, totp_maker::totp,
        yandex_otp_maker::yandex,
    },
    migrations::migrate,
    otp_algorithm::OTPAlgorithm,
    otp_type::OTPType,
};

pub const CURRENT_DATABASE_VERSION: u16 = 2;

#[derive(Serialize, Deserialize)]
pub struct OTPDatabase {
    pub(crate) version: u16,
    pub(crate) elements: Vec<OTPElement>,
    #[serde(skip)]
    pub(crate) needs_modification: bool,
}

impl Default for OTPDatabase {
    fn default() -> Self {
        Self {
            version: CURRENT_DATABASE_VERSION,
            elements: vec![],
            needs_modification: false,
        }
    }
}

impl OTPDatabase {
    pub fn is_modified(&self) -> bool {
        self.needs_modification
    }

    pub fn save(&mut self, key: &Vec<u8>, salt: &[u8]) -> Result<(), String> {
        self.needs_modification = false;
        migrate(self)?;
        match self.overwrite_database_key(key, salt) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    fn overwrite_database_key(&self, key: &Vec<u8>, salt: &[u8]) -> Result<(), std::io::Error> {
        let json: &str = &serde_json::to_string(&self)?;
        let encrypted = encrypt_string_with_key(json.to_string(), key, salt).unwrap();
        let mut file = File::create(utils::get_db_path())?;
        match serde_json::to_string(&encrypted) {
            Ok(content) => {
                file.write_all(content.as_bytes())?;
                file.sync_all()?;
                Ok(())
            }
            Err(e) => Err(std::io::Error::from(e)),
        }
    }

    pub fn save_with_pw(&mut self, password: &str) -> Result<(), String> {
        let salt = gen_salt()?;
        let key = argon_derive_key(password.as_bytes(), &salt)?;
        self.save(&key, &salt)
    }

    pub fn export(&self, path: PathBuf) -> Result<PathBuf, String> {
        if self.elements.is_empty() {
            return Err(String::from(
                "there are no elements in your database, type \"cotp -h\" to get help",
            ));
        }

        let exported_path = if path.is_dir() {
            path.join("exported.cotp")
        } else {
            path
        };

        match serde_json::to_string(self) {
            Ok(mut contents) => {
                if contents == "[]" {}
                let mut file = File::create(&exported_path).expect("Cannot create file");
                let contents_bytes = contents.as_bytes();
                file.write_all(contents_bytes)
                    .expect("Failed to write contents");
                contents.zeroize();
                Ok(exported_path)
            }
            Err(e) => Err(format!("{:?}", e)),
        }
    }

    pub fn add_all(&mut self, mut elements: Vec<OTPElement>) {
        self.needs_modification = true;
        self.elements.append(&mut elements)
    }

    pub fn add_element(&mut self, element: OTPElement) {
        self.needs_modification = true;
        self.elements.push(element)
    }

    pub fn edit_element(&mut self, index: usize, element: OTPElement) {
        self.needs_modification = true;
        self.elements[index] = element;
    }

    pub fn delete_element(&mut self, index: usize) {
        self.needs_modification = true;
        self.elements.remove(index);
    }

    pub fn elements_ref(&self) -> &[OTPElement] {
        &self.elements
    }

    pub fn get_element(&self, i: usize) -> Option<&OTPElement> {
        self.elements.get(i)
    }

    pub fn mut_element(&mut self, i: usize) -> Option<&mut OTPElement> {
        self.elements.get_mut(i)
    }

    pub fn sort(&mut self) {
        self.elements
            .sort_unstable_by(|c1, c2| c1.issuer.cmp(&c2.issuer))
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct OTPElement {
    pub secret: String,
    pub issuer: String,
    pub label: String,
    pub digits: u64,
    #[serde(rename = "type")]
    pub type_: OTPType,
    pub algorithm: OTPAlgorithm,
    pub period: u64,
    pub counter: Option<u64>,
    pub pin: Option<String>,
}

impl OTPElement {
    pub fn get_otpauth_uri(&self) -> String {
        let mut uri: String = String::from("otpauth://");
        uri.push_str(self.type_.to_string().to_lowercase().as_str());
        uri.push('/');
        //self.type_.to_lowercase() + String::from("/");
        if self.issuer.chars().count() > 0 {
            uri.push_str(&urlencoding::encode(self.issuer.as_str()));
            uri.push(':');
        }
        uri.push_str(&urlencoding::encode(self.label.as_str()));

        uri.push_str("?secret=");
        uri.push_str(self.secret.as_str());
        uri.push_str("&algorithm=");
        uri.push_str(self.algorithm.to_string().to_uppercase().as_str());
        uri.push_str("&digits=");
        uri.push_str(self.digits.to_string().as_str());
        uri.push_str("&period=");
        uri.push_str(self.period.to_string().as_str());
        uri.push_str("&lock=false");
        //uri.push_str("?secret=" + self.secret());
        if self.type_ == OTPType::Hotp {
            uri.push_str("&counter=");
            uri.push_str(self.counter.unwrap_or(0).to_string().as_str());
        }
        uri
    }

    pub fn get_qrcode(&self) -> String {
        QrCode::new(&self.get_otpauth_uri())
            .unwrap()
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build()
    }

    pub fn get_otp_code(&self) -> Result<String, String> {
        match self.type_ {
            OTPType::Totp => {
                let code = totp(&self.secret, self.algorithm)?;

                Ok(self.format_code(code))
            }
            OTPType::Hotp => match self.counter {
                Some(counter) => {
                    let code = hotp(&self.secret, self.algorithm, counter)?;

                    Ok(self.format_code(code))
                }
                None => Err(String::from(
                    "The element is an HOTP code but there is no counter value.",
                )),
            },
            OTPType::Steam => steam(&self.secret, self.algorithm, self.digits as usize),
            OTPType::Yandex => match &self.pin {
                Some(pin) => yandex(
                    &self.secret,
                    pin.as_str(),
                    self.period,
                    self.digits as usize,
                    self.algorithm,
                ),
                None => Err(String::from(
                    "This element is a Yandex code but there is not pin value",
                )),
            },
            OTPType::Motp => match &self.pin {
                Some(pin) => motp(
                    &self.secret,
                    pin.as_str(),
                    self.period as u64,
                    self.digits as usize,
                ),
                None => Err(String::from(
                    "This element is an MOTP code but the is not pin value",
                )),
            },
        }
    }

    pub fn format_code(&self, value: u32) -> String {
        // Get the formatted code
        let s = (value % 10_u32.pow(self.digits as u32)).to_string();
        "0".repeat(self.digits as usize - s.chars().count()) + s.as_str()
    }

    pub fn valid_secret(&self) -> bool {
        match self.type_ {
            OTPType::Motp => hex::decode(&self.secret).is_ok(),
            _ => BASE32_NOPAD.decode(self.secret.as_bytes()).is_ok(),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::otp::otp_element::OTPAlgorithm::Sha1;
    use crate::otp::otp_element::OTPElement;
    use crate::otp::otp_element::OTPType::Totp;

    #[test]
    fn test_otpauth_uri() {
        let otp_element = OTPElement {
            secret: String::from("xr5gh44x7bprcqgrdtulafeevt5rxqlbh5wvked22re43dh2d4mapv5g"),
            issuer: String::from("IssuerText"),
            label: String::from("LabelText"),
            digits: 6,
            type_: Totp,
            algorithm: Sha1,
            period: 30,
            counter: None,
            pin: None,
        };
        assert_eq!(otp_element.get_otpauth_uri().as_str(), "otpauth://totp/IssuerText:LabelText?secret=xr5gh44x7bprcqgrdtulafeevt5rxqlbh5wvked22re43dh2d4mapv5g&algorithm=SHA1&digits=6&period=30&lock=false");
    }
}
