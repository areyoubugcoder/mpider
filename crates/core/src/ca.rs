//! 根 CA 生成与 hudsucker 授权体（对齐 Python 版“rcgen 生成根 CA + 动态签叶证书”思路）。
//!
//! 运行期用 `rcgen` 生成一个自签根 CA，落 PEM（供“证书信任向导”安装到系统信任库、
//! 以及单测里让 reqwest 信任），再用它构造 hudsucker 的 [`RcgenAuthority`]，由后者在
//! MITM 握手时为每个目标 host 动态签叶证书。
//!
//! 版本对齐：统一走 `hudsucker::rcgen` / `hudsucker::rustls` 再导出，避免与 hudsucker
//! 内部依赖的 rcgen/rustls 版本错配。

use anyhow::Result;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use hudsucker::rustls::crypto::aws_lc_rs;

/// 生成出的根 CA 材料（PEM）。`cert_pem` 是要被客户端信任的根证书；`key_pem` 是其私钥。
#[derive(Clone, Debug)]
pub struct CaMaterial {
    pub cert_pem: String,
    pub key_pem: String,
}

/// 生成一个新的自签根 CA（ECDSA P-256）。
pub fn generate_ca() -> Result<CaMaterial> {
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "MPider MITM CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "mpider");
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let cert = params.self_signed(&key_pair)?;
    Ok(CaMaterial {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
}

impl CaMaterial {
    /// 生成一个新的自签根 CA（[`generate_ca`] 的关联函数别名）。
    pub fn generate() -> Result<Self> {
        generate_ca()
    }

    /// 从已有 PEM 载入（例如从 `data/` 里读回持久化的 CA）。
    pub fn from_pem(cert_pem: impl Into<String>, key_pem: impl Into<String>) -> Self {
        Self {
            cert_pem: cert_pem.into(),
            key_pem: key_pem.into(),
        }
    }

    /// 构造 hudsucker 的 rcgen 授权体（用 aws-lc-rs 作为 rustls crypto provider）。
    ///
    /// 用 [`Issuer::new`] 由 CA 参数 + 私钥直接得到一个自持有的 `Issuer<'static, _>`，
    /// 无需 x509-parser 解析 PEM。
    pub fn authority(&self) -> Result<RcgenAuthority> {
        let key_pair = KeyPair::from_pem(&self.key_pem)?;
        // 重建与自签 CA 一致的参数（DN / key_usages），使签出的叶证书 issuer 与根一致。
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "MPider MITM CA");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "mpider");
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer: Issuer<'static, KeyPair> = Issuer::new(params, key_pair);
        Ok(RcgenAuthority::new(
            issuer,
            1_000,
            aws_lc_rs::default_provider(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_ca_pem_shapes() {
        let ca = generate_ca().unwrap();
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.key_pem.contains("PRIVATE KEY"));
        // 能据此构造授权体（不 panic）。
        let _authority = ca.authority().unwrap();
    }
}
