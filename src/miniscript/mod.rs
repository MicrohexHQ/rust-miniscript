// Miniscript
// Written in 2019 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! # AST Tree
//!
//! Defines a variety of data structures for describing Miniscript, a subset of
//! Bitcoin Script which can be efficiently parsed and serialized from Script,
//! and from which it is easy to extract data needed to construct witnesses.
//!
//! Users of the library in general will only need to use the structures exposed
//! from the top level of this module; however for people wanting to do advanced
//! things, the submodules are public as well which provide visibility into the
//! components of the AST trees.
//!

#[cfg(feature = "serde")]
use serde::{de, ser};
use std::{fmt, str};

use bitcoin;
use bitcoin::blockdata::script;

pub mod astelem;
pub mod decode;
pub mod lex;
pub mod satisfy;
pub mod types;

use self::lex::{lex, TokenIter};
use self::types::Property;
use miniscript::types::extra_props::ExtData;
use miniscript::types::Type;
use std::cmp;
use std::sync::Arc;
use MiniscriptKey;
use {expression, Error, ToPublicKey};

/// Top-level script AST type
#[derive(Clone, Hash)]
pub struct Miniscript<Pk: MiniscriptKey> {
    ///A node in the Abstract Syntax Tree(
    pub node: decode::Terminal<Pk>,
    ///The correctness and malleability type information for the AST node
    pub ty: types::Type,
    ///Additional information helpful for extra analysis.
    pub ext: types::extra_props::ExtData,
}

/// `PartialOrd` of `Miniscript` must depend only on node and not the type information.
/// The type information and extra_properties can be deterministically determined
/// by the ast tree.
impl<Pk: MiniscriptKey> PartialOrd for Miniscript<Pk> {
    fn partial_cmp(&self, other: &Miniscript<Pk>) -> Option<cmp::Ordering> {
        Some(self.node.cmp(&other.node))
    }
}

/// `Ord` of `Miniscript` must depend only on node and not the type information.
/// The type information and extra_properties can be deterministically determined
/// by the ast tree.
impl<Pk: MiniscriptKey> Ord for Miniscript<Pk> {
    fn cmp(&self, other: &Miniscript<Pk>) -> cmp::Ordering {
        self.node.cmp(&other.node)
    }
}

/// `PartialEq` of `Miniscript` must depend only on node and not the type information.
/// The type information and extra_properties can be deterministically determined
/// by the ast tree.
impl<Pk: MiniscriptKey> PartialEq for Miniscript<Pk> {
    fn eq(&self, other: &Miniscript<Pk>) -> bool {
        self.node.eq(&other.node)
    }
}

/// `Eq` of `Miniscript` must depend only on node and not the type information.
/// The type information and extra_properties can be deterministically determined
/// by the ast tree.
impl<Pk: MiniscriptKey> Eq for Miniscript<Pk> {}

impl<Pk: MiniscriptKey> fmt::Debug for Miniscript<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.node)
    }
}

impl<Pk: MiniscriptKey> Miniscript<Pk> {
    /// Add type information(Type and Extdata) to Miniscript based on
    /// `AstElem` fragment. Dependent on display and clone because of Error
    /// Display code of type_check.
    pub fn from_ast(t: decode::Terminal<Pk>) -> Result<Miniscript<Pk>, Error> {
        Ok(Miniscript {
            ty: Type::type_check(&t, |_| None)?,
            ext: ExtData::type_check(&t, |_| None)?,
            node: t,
        })
    }
}

impl<Pk: MiniscriptKey> fmt::Display for Miniscript<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.node)
    }
}

impl<Pk: MiniscriptKey> Miniscript<Pk> {
    /// Extracts the `AstElem` representing the root of the miniscript
    pub fn into_inner(self) -> decode::Terminal<Pk> {
        self.node
    }

    pub fn as_inner(&self) -> &decode::Terminal<Pk> {
        &self.node
    }
}

impl Miniscript<bitcoin::PublicKey> {
    /// Attempt to parse a script into a Miniscript representation
    pub fn parse(script: &script::Script) -> Result<Miniscript<bitcoin::PublicKey>, Error> {
        let tokens = lex(script)?;
        let mut iter = TokenIter::new(tokens);

        let top = decode::parse(&mut iter)?;
        let type_check = types::Type::type_check(&top.node, |_| None)?;
        if type_check.corr.base != types::Base::B {
            return Err(Error::NonTopLevel(format!("{:?}", top)));
        };
        if let Some(leading) = iter.next() {
            Err(Error::Trailing(leading.to_string()))
        } else {
            Ok(top)
        }
    }
}

impl<Pk: MiniscriptKey + ToPublicKey> Miniscript<Pk> {
    /// Encode as a Bitcoin script
    pub fn encode(&self) -> script::Script {
        self.node.encode(script::Builder::new()).into_script()
    }

    /// Size, in bytes of the script-pubkey. If this Miniscript is used outside
    /// of segwit (e.g. in a bare or P2SH descriptor), this quantity should be
    /// multiplied by 4 to compute the weight.
    ///
    /// In general, it is not recommended to use this function directly, but
    /// to instead call the corresponding function on a `Descriptor`, which
    /// will handle the segwit/non-segwit technicalities for you.
    pub fn script_size(&self) -> usize {
        self.node.script_size()
    }

    /// Maximum number of witness elements used to satisfy the Miniscript
    /// fragment, including the witness script itself. Used to estimate
    /// the weight of the `VarInt` that specifies this number in a serialized
    /// transaction.
    ///
    /// This function may panic on misformed `Miniscript` objects which do
    /// not correspond to semantically sane Scripts. (Such scripts should be
    /// rejected at parse time. Any exceptions are bugs.)
    pub fn max_satisfaction_witness_elements(&self) -> usize {
        1 + self.node.max_satisfaction_witness_elements()
    }

    /// Maximum size, in bytes, of a satisfying witness. For Segwit outputs
    /// `one_cost` should be set to 2, since the number `1` requires two
    /// bytes to encode. For non-segwit outputs `one_cost` should be set to
    /// 1, since `OP_1` is available in scriptSigs.
    ///
    /// In general, it is not recommended to use this function directly, but
    /// to instead call the corresponding function on a `Descriptor`, which
    /// will handle the segwit/non-segwit technicalities for you.
    ///
    /// All signatures are assumed to be 73 bytes in size, including the
    /// length prefix (segwit) or push opcode (pre-segwit) and sighash
    /// postfix.
    ///
    /// This function may panic on misformed `Miniscript` objects which do not
    /// correspond to semantically sane Scripts. (Such scripts should be
    /// rejected at parse time. Any exceptions are bugs.)
    pub fn max_satisfaction_size(&self, one_cost: usize) -> usize {
        self.node.max_satisfaction_size(one_cost)
    }
}

impl<Pk: MiniscriptKey> Miniscript<Pk> {
    pub fn translate_pk<FPk, FPkh, Q, Error>(
        &self,
        translatefpk: &mut FPk,
        translatefpkh: &mut FPkh,
    ) -> Result<Miniscript<Q>, Error>
    where
        FPk: FnMut(&Pk) -> Result<Q, Error>,
        FPkh: FnMut(&Pk::Hash) -> Result<Q::Hash, Error>,
        Q: MiniscriptKey,
    {
        let inner = self.node.translate_pk(translatefpk, translatefpkh)?;
        Ok(Miniscript {
            //directly copying the type and ext is safe because translating public
            //key should not change any properties
            ty: self.ty,
            ext: self.ext,
            node: inner,
        })
    }
}

impl<Pk: MiniscriptKey + ToPublicKey> Miniscript<Pk> {
    /// Attempt to produce a satisfying witness for the
    /// witness script represented by the parse tree
    pub fn satisfy<S: satisfy::Satisfier<Pk>>(&self, satisfier: S) -> Option<Vec<Vec<u8>>> {
        match satisfy::Satisfaction::satisfy(&self.node, &satisfier).stack {
            satisfy::Witness::Stack(stack) => Some(stack),
            satisfy::Witness::Unavailable => None,
        }
    }
}

impl<Pk> expression::FromTree for Arc<Miniscript<Pk>>
where
    Pk: MiniscriptKey,
    <Pk as str::FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
{
    fn from_tree(top: &expression::Tree) -> Result<Arc<Miniscript<Pk>>, Error> {
        Ok(Arc::new(expression::FromTree::from_tree(top)?))
    }
}

impl<Pk> expression::FromTree for Miniscript<Pk>
where
    Pk: MiniscriptKey,
    <Pk as str::FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
{
    /// Parse an expression tree into a Miniscript. As a general rule, this
    /// should not be called directly; rather go through the descriptor API.
    fn from_tree(top: &expression::Tree) -> Result<Miniscript<Pk>, Error> {
        let inner: decode::Terminal<Pk> = expression::FromTree::from_tree(top)?;
        Ok(Miniscript {
            ty: Type::type_check(&inner, |_| None)?,
            ext: ExtData::type_check(&inner, |_| None)?,
            node: inner,
        })
    }
}

impl<Pk> str::FromStr for Miniscript<Pk>
where
    Pk: MiniscriptKey,
    <Pk as str::FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
{
    type Err = Error;

    fn from_str(s: &str) -> Result<Miniscript<Pk>, Error> {
        for ch in s.as_bytes() {
            if *ch < 20 || *ch > 127 {
                return Err(Error::Unprintable(*ch));
            }
        }

        let top = expression::Tree::from_str(s)?;
        let ms: Miniscript<Pk> = expression::FromTree::from_tree(&top)?;

        if ms.ty.corr.base != types::Base::B {
            Err(Error::NonTopLevel(format!("{:?}", ms)))
        } else {
            Ok(ms)
        }
    }
}

#[cfg(feature = "serde")]
impl<Pk: MiniscriptKey> ser::Serialize for Miniscript<Pk> where {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de, Pk> de::Deserialize<'de> for Miniscript<Pk>
where
    Pk: MiniscriptKey,
    <Pk as str::FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
{
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Miniscript<Pk>, D::Error> {
        use std::marker::PhantomData;
        use std::str::FromStr;

        struct StrVisitor<Qk>(PhantomData<(Qk)>);

        impl<'de, Qk> de::Visitor<'de> for StrVisitor<Qk>
        where
            Qk: MiniscriptKey,
            <Qk as str::FromStr>::Err: ToString,
            <<Qk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
        {
            type Value = Miniscript<Qk>;

            fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
                fmt.write_str("an ASCII miniscript string")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if let Ok(s) = str::from_utf8(v) {
                    Miniscript::from_str(s).map_err(E::custom)
                } else {
                    return Err(E::invalid_value(de::Unexpected::Bytes(v), &self));
                }
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Miniscript::from_str(v).map_err(E::custom)
            }
        }

        d.deserialize_str(StrVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::Miniscript;
    use hex_script;
    use miniscript::decode::Terminal;
    use miniscript::types::{self, ExtData, Property, Type};
    use policy::Liftable;
    use DummyKey;
    use DummyKeyHash;

    use bitcoin::hashes::{hash160, sha256, Hash};
    use bitcoin::{self, secp256k1};
    use std::str;
    use std::str::FromStr;
    use std::sync::Arc;
    use MiniscriptKey;

    type BScript = Miniscript<bitcoin::PublicKey>;

    fn pubkeys(n: usize) -> Vec<bitcoin::PublicKey> {
        let mut ret = Vec::with_capacity(n);
        let secp = secp256k1::Secp256k1::new();
        let mut sk = [0; 32];
        for i in 1..n + 1 {
            sk[0] = i as u8;
            sk[1] = (i >> 8) as u8;
            sk[2] = (i >> 16) as u8;

            let pk = bitcoin::PublicKey {
                key: secp256k1::PublicKey::from_secret_key(
                    &secp,
                    &secp256k1::SecretKey::from_slice(&sk[..]).expect("secret key"),
                ),
                compressed: true,
            };
            ret.push(pk);
        }
        ret
    }

    fn string_rtt<Pk, Str1, Str2>(
        script: Miniscript<Pk>,
        expected_debug: Str1,
        expected_display: Str2,
    ) where
        Pk: MiniscriptKey,
        <Pk as str::FromStr>::Err: ToString,
        <<Pk as MiniscriptKey>::Hash as str::FromStr>::Err: ToString,
        Str1: Into<Option<&'static str>>,
        Str2: Into<Option<&'static str>>,
    {
        assert_eq!(script.ty.corr.base, types::Base::B);
        let debug = format!("{:?}", script);
        let display = format!("{}", script);
        if let Some(expected) = expected_debug.into() {
            assert_eq!(debug, expected);
        }
        if let Some(expected) = expected_display.into() {
            assert_eq!(display, expected);
        }
        let roundtrip = Miniscript::from_str(&display).expect("parse string serialization");
        assert_eq!(roundtrip, script);

        let translated: Result<_, ()> =
            script.translate_pk(&mut |k| Ok(k.clone()), &mut |h| Ok(h.clone()));
        assert_eq!(translated, Ok(script));
    }

    fn script_rtt<Str1: Into<Option<&'static str>>>(script: BScript, expected_hex: Str1) {
        assert_eq!(script.ty.corr.base, types::Base::B);
        let bitcoin_script = script.encode();
        assert_eq!(bitcoin_script.len(), script.script_size());
        if let Some(expected) = expected_hex.into() {
            assert_eq!(format!("{:x}", bitcoin_script), expected);
        }
        let roundtrip = Miniscript::parse(&bitcoin_script).expect("parse string serialization");
        assert_eq!(roundtrip, script);
    }

    fn roundtrip(tree: &Miniscript<bitcoin::PublicKey>, s: &str) {
        assert_eq!(tree.ty.corr.base, types::Base::B);
        let ser = tree.encode();
        assert_eq!(ser.len(), tree.script_size());
        assert_eq!(ser.to_string(), s);
        let deser = Miniscript::parse(&ser).expect("deserialize result of serialize");
        assert_eq!(*tree, deser);
    }

    fn ms_attributes_test(
        ms: &str,
        expected_hex: &str,
        valid: bool,
        non_mal: bool,
        need_sig: bool,
        ops: usize,
        _stack: usize,
    ) {
        let ms: Result<Miniscript<bitcoin::PublicKey>, _> = Miniscript::from_str(ms);
        match (ms, valid) {
            (Ok(ms), true) => {
                assert_eq!(format!("{:x}", ms.encode()), expected_hex);
                assert_eq!(ms.ty.mall.non_malleable, non_mal);
                assert_eq!(ms.ty.mall.safe, need_sig);
                assert_eq!(ms.ext.ops_count_sat.unwrap(), ops);
            }
            (Err(_), false) => return,
            _ => unreachable!(),
        }
    }

    #[test]
    fn all_attribute_tests() {
        ms_attributes_test(
            "lltvln:after(1231488000)",
            "6300676300676300670400046749b1926869516868",
            true,
            true,
            false,
            12,
            3,
        );
        ms_attributes_test("uuj:and_v(v:thresh_m(2,03d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a,025601570cb47f238d2b0286db4a990fa0f3ba28d1a319f5e7cf55c2a2444da7cc),after(1231488000))", "6363829263522103d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a21025601570cb47f238d2b0286db4a990fa0f3ba28d1a319f5e7cf55c2a2444da7cc52af0400046749b168670068670068", true, true, true, 14, 5);
        ms_attributes_test("or_b(un:thresh_m(2,03daed4f2be3a8bf278e70132fb0beb7522f570e144bf615c07e996d443dee8729,024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c97),al:older(16))", "63522103daed4f2be3a8bf278e70132fb0beb7522f570e144bf615c07e996d443dee872921024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c9752ae926700686b63006760b2686c9b", true, false, false, 14, 5);
        ms_attributes_test(
            "j:and_v(vdv:after(1567547623),older(2016))",
            "829263766304e7e06e5db169686902e007b268",
            true,
            true,
            false,
            11,
            1,
        );
        ms_attributes_test("t:and_v(vu:hash256(131772552c01444cd81360818376a040b7c3b2b7b0a53550ee3edde216cec61b),v:sha256(ec4916dd28fc4c10d78e287ca5d9cc51ee1ae73cbfde08c6b37324cbfaac8bc5))", "6382012088aa20131772552c01444cd81360818376a040b7c3b2b7b0a53550ee3edde216cec61b876700686982012088a820ec4916dd28fc4c10d78e287ca5d9cc51ee1ae73cbfde08c6b37324cbfaac8bc58851", true, true, false, 12, 3);
        ms_attributes_test("t:andor(thresh_m(3,02d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e,03fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556,02e493dbf1c10d80f3581e4904930b1404cc6c13900ee0758474fa94abe8c4cd13),v:older(4194305),v:sha256(9267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed2))", "532102d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e2103fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a14602975562102e493dbf1c10d80f3581e4904930b1404cc6c13900ee0758474fa94abe8c4cd1353ae6482012088a8209267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed2886703010040b2696851", true, true, false, 13, 5);
        ms_attributes_test("or_d(thresh_m(1,02f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9),or_b(thresh_m(3,022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a01,032fa2104d6b38d11b0230010559879124e42ab8dfeff5ff29dc9cdadd4ecacc3f,03d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a),su:after(500000)))", "512102f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f951ae73645321022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a0121032fa2104d6b38d11b0230010559879124e42ab8dfeff5ff29dc9cdadd4ecacc3f2103d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a53ae7c630320a107b16700689b68", true, true, false, 15, 7);
        ms_attributes_test("or_d(sha256(38df1c1f64a24a77b23393bca50dff872e31edc4f3b5aa3b90ad0b82f4f089b6),and_n(un:after(499999999),older(4194305)))", "82012088a82038df1c1f64a24a77b23393bca50dff872e31edc4f3b5aa3b90ad0b82f4f089b68773646304ff64cd1db19267006864006703010040b26868", true, false, false, 16, 1);
        ms_attributes_test("and_v(or_i(v:thresh_m(2,02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5,03774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb),v:thresh_m(2,03e60fce93b59e9ec53011aabc21c23e97b2a31369b87a5ae9c44ee89e2a6dec0a,025cbdf0646e5db4eaa398f365f2ea7a0e3d419b7e0330e39ce92bddedcac4f9bc)),sha256(d1ec675902ef1633427ca360b290b0b3045a0d9058ddb5e648b4c3c3224c5c68))", "63522102c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee52103774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb52af67522103e60fce93b59e9ec53011aabc21c23e97b2a31369b87a5ae9c44ee89e2a6dec0a21025cbdf0646e5db4eaa398f365f2ea7a0e3d419b7e0330e39ce92bddedcac4f9bc52af6882012088a820d1ec675902ef1633427ca360b290b0b3045a0d9058ddb5e648b4c3c3224c5c6887", true, true, true, 11, 5);
        ms_attributes_test("j:and_b(thresh_m(2,0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798,024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c97),s:or_i(older(1),older(4252898)))", "82926352210279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f8179821024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c9752ae7c6351b26703e2e440b2689a68", true, false, true, 14, 4);
        ms_attributes_test("and_b(older(16),s:or_d(sha256(e38990d0c7fc009880a9c07c23842e886c6bbdc964ce6bdd5817ad357335ee6f),n:after(1567547623)))", "60b27c82012088a820e38990d0c7fc009880a9c07c23842e886c6bbdc964ce6bdd5817ad357335ee6f87736404e7e06e5db192689a", true, false, false, 12, 1);
        ms_attributes_test("j:and_v(v:hash160(20195b5a3d650c17f0f29f91c33f8f6335193d07),or_d(sha256(96de8fc8c256fa1e1556d41af431cace7dca68707c78dd88c3acab8b17164c47),older(16)))", "82926382012088a91420195b5a3d650c17f0f29f91c33f8f6335193d078882012088a82096de8fc8c256fa1e1556d41af431cace7dca68707c78dd88c3acab8b17164c4787736460b26868", true, false, false, 16, 2);
        ms_attributes_test("and_b(hash256(32ba476771d01e37807990ead8719f08af494723de1d228f2c2c07cc0aa40bac),a:and_b(hash256(131772552c01444cd81360818376a040b7c3b2b7b0a53550ee3edde216cec61b),a:older(1)))", "82012088aa2032ba476771d01e37807990ead8719f08af494723de1d228f2c2c07cc0aa40bac876b82012088aa20131772552c01444cd81360818376a040b7c3b2b7b0a53550ee3edde216cec61b876b51b26c9a6c9a", true, true, false, 15, 2);
        ms_attributes_test("thresh(2,thresh_m(2,03a0434d9e47f3c86235477c7b1ae6ae5d3442d49b1943c2b752a68e2a47e247c7,036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a00),a:thresh_m(1,036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a00),ac:pk(022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a01))", "522103a0434d9e47f3c86235477c7b1ae6ae5d3442d49b1943c2b752a68e2a47e247c721036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a0052ae6b5121036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a0051ae6c936b21022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a01ac6c935287", true, true, true, 13, 6);
        ms_attributes_test("and_n(sha256(d1ec675902ef1633427ca360b290b0b3045a0d9058ddb5e648b4c3c3224c5c68),t:or_i(v:older(4252898),v:older(144)))", "82012088a820d1ec675902ef1633427ca360b290b0b3045a0d9058ddb5e648b4c3c3224c5c68876400676303e2e440b26967029000b269685168", true, false, false, 14, 2);
        ms_attributes_test("or_d(d:and_v(v:older(4252898),v:older(4252898)),sha256(38df1c1f64a24a77b23393bca50dff872e31edc4f3b5aa3b90ad0b82f4f089b6))", "766303e2e440b26903e2e440b26968736482012088a82038df1c1f64a24a77b23393bca50dff872e31edc4f3b5aa3b90ad0b82f4f089b68768", true, false, false, 14, 2);
        ms_attributes_test("c:and_v(or_c(sha256(9267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed2),v:thresh_m(1,02c44d12c7065d812e8acf28d7cbb19f9011ecd9e9fdf281b0e6a3b5e87d22e7db)),pk(03acd484e2f0c7f65309ad178a9f559abde09796974c57e714c35f110dfc27ccbe))", "82012088a8209267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed28764512102c44d12c7065d812e8acf28d7cbb19f9011ecd9e9fdf281b0e6a3b5e87d22e7db51af682103acd484e2f0c7f65309ad178a9f559abde09796974c57e714c35f110dfc27ccbeac", true, false, true, 8, 2);
        ms_attributes_test("c:and_v(or_c(thresh_m(2,036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a00,02352bbf4a4cdd12564f93fa332ce333301d9ad40271f8107181340aef25be59d5),v:ripemd160(1b0f3c404d12075c68c938f9f60ebea4f74941a0)),pk(03fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556))", "5221036d2b085e9e382ed10b69fc311a03f8641ccfff21574de0927513a49d9a688a002102352bbf4a4cdd12564f93fa332ce333301d9ad40271f8107181340aef25be59d552ae6482012088a6141b0f3c404d12075c68c938f9f60ebea4f74941a088682103fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556ac", true, true, true, 10, 5);
        ms_attributes_test("and_v(andor(hash256(8a35d9ca92a48eaade6f53a64985e9e2afeb74dcf8acb4c3721e0dc7e4294b25),v:hash256(939894f70e6c3a25da75da0cc2071b4076d9b006563cf635986ada2e93c0d735),v:older(50000)),after(499999999))", "82012088aa208a35d9ca92a48eaade6f53a64985e9e2afeb74dcf8acb4c3721e0dc7e4294b2587640350c300b2696782012088aa20939894f70e6c3a25da75da0cc2071b4076d9b006563cf635986ada2e93c0d735886804ff64cd1db1", true, false, false, 14, 2);
        ms_attributes_test("andor(hash256(5f8d30e655a7ba0d7596bb3ddfb1d2d20390d23b1845000e1e118b3be1b3f040),j:and_v(v:hash160(3a2bff0da9d96868e66abc4427bea4691cf61ccd),older(4194305)),ripemd160(44d90e2d3714c8663b632fcf0f9d5f22192cc4c8))", "82012088aa205f8d30e655a7ba0d7596bb3ddfb1d2d20390d23b1845000e1e118b3be1b3f040876482012088a61444d90e2d3714c8663b632fcf0f9d5f22192cc4c8876782926382012088a9143a2bff0da9d96868e66abc4427bea4691cf61ccd8803010040b26868", true, false, false, 20, 2);
        ms_attributes_test("or_i(c:and_v(v:after(500000),pk(02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5)),sha256(d9147961436944f43cd99d28b2bbddbf452ef872b30c8279e255e7daafc7f946))", "630320a107b1692102c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5ac6782012088a820d9147961436944f43cd99d28b2bbddbf452ef872b30c8279e255e7daafc7f9468768", true, true, false, 10, 2);
        ms_attributes_test("thresh(2,c:pk_h(5dedfbf9ea599dd4e3ca6a80b333c472fd0b3f69),s:sha256(e38990d0c7fc009880a9c07c23842e886c6bbdc964ce6bdd5817ad357335ee6f),a:hash160(dd69735817e0e3f6f826a9238dc2e291184f0131))", "76a9145dedfbf9ea599dd4e3ca6a80b333c472fd0b3f6988ac7c82012088a820e38990d0c7fc009880a9c07c23842e886c6bbdc964ce6bdd5817ad357335ee6f87936b82012088a914dd69735817e0e3f6f826a9238dc2e291184f0131876c935287", true, false, false, 18, 4);
        ms_attributes_test("and_n(sha256(9267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed2),uc:and_v(v:older(144),pk(03fe72c435413d33d48ac09c9161ba8b09683215439d62b7940502bda8b202e6ce)))", "82012088a8209267d3dbed802941483f1afa2a6bc68de5f653128aca9bf1461c5d0a3ad36ed28764006763029000b2692103fe72c435413d33d48ac09c9161ba8b09683215439d62b7940502bda8b202e6ceac67006868", true, false, true, 13, 3);
        ms_attributes_test("and_n(c:pk(03daed4f2be3a8bf278e70132fb0beb7522f570e144bf615c07e996d443dee8729),and_b(l:older(4252898),a:older(16)))", "2103daed4f2be3a8bf278e70132fb0beb7522f570e144bf615c07e996d443dee8729ac64006763006703e2e440b2686b60b26c9a68", true, true, true, 12, 2);
        ms_attributes_test("c:or_i(and_v(v:older(16),pk_h(9fc5dbe5efdce10374a4dd4053c93af540211718)),pk_h(2fbd32c8dd59ee7c17e66cb6ebea7e9846c3040f))", "6360b26976a9149fc5dbe5efdce10374a4dd4053c93af540211718886776a9142fbd32c8dd59ee7c17e66cb6ebea7e9846c3040f8868ac", true, true, true, 12, 3);
        ms_attributes_test("or_d(c:pk_h(c42e7ef92fdb603af844d064faad95db9bcdfd3d),andor(c:pk(024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c97),older(2016),after(1567547623)))", "76a914c42e7ef92fdb603af844d064faad95db9bcdfd3d88ac736421024ce119c96e2fa357200b559b2f7dd5a5f02d5290aff74b03f3e471b273211c97ac6404e7e06e5db16702e007b26868", true, true, false, 13, 3);
        ms_attributes_test("c:andor(ripemd160(6ad07d21fd5dfc646f0b30577045ce201616b9ba),pk_h(9fc5dbe5efdce10374a4dd4053c93af540211718),and_v(v:hash256(8a35d9ca92a48eaade6f53a64985e9e2afeb74dcf8acb4c3721e0dc7e4294b25),pk_h(dd100be7d9aea5721158ebde6d6a1fd8fff93bb1)))", "82012088a6146ad07d21fd5dfc646f0b30577045ce201616b9ba876482012088aa208a35d9ca92a48eaade6f53a64985e9e2afeb74dcf8acb4c3721e0dc7e4294b258876a914dd100be7d9aea5721158ebde6d6a1fd8fff93bb1886776a9149fc5dbe5efdce10374a4dd4053c93af5402117188868ac", true, false, true, 18, 3);
        ms_attributes_test("c:andor(u:ripemd160(6ad07d21fd5dfc646f0b30577045ce201616b9ba),pk_h(20d637c1a6404d2227f3561fdbaff5a680dba648),or_i(pk_h(9652d86bedf43ad264362e6e6eba6eb764508127),pk_h(751e76e8199196d454941c45d1b3a323f1433bd6)))", "6382012088a6146ad07d21fd5dfc646f0b30577045ce201616b9ba87670068646376a9149652d86bedf43ad264362e6e6eba6eb764508127886776a914751e76e8199196d454941c45d1b3a323f1433bd688686776a91420d637c1a6404d2227f3561fdbaff5a680dba6488868ac", true, false, true, 23, 4);
        ms_attributes_test("c:or_i(andor(c:pk_h(fcd35ddacad9f2d5be5e464639441c6065e6955d),pk_h(9652d86bedf43ad264362e6e6eba6eb764508127),pk_h(06afd46bcdfd22ef94ac122aa11f241244a37ecc)),pk(02d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e))", "6376a914fcd35ddacad9f2d5be5e464639441c6065e6955d88ac6476a91406afd46bcdfd22ef94ac122aa11f241244a37ecc886776a9149652d86bedf43ad264362e6e6eba6eb7645081278868672102d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e68ac", true, true, true, 17, 5);
    }

    #[test]
    fn basic() {
        let pk = bitcoin::PublicKey::from_str(
            "\
             020202020202020202020202020202020202020202020202020202020202020202\
             ",
        )
        .unwrap();
        let hash = hash160::Hash::from_inner([17; 20]);

        let pk_ms: Miniscript<DummyKey> = Miniscript {
            node: Terminal::Check(Arc::new(Miniscript {
                node: Terminal::Pk(DummyKey),
                ty: Type::from_pk(),
                ext: types::extra_props::ExtData::from_pk(),
            })),
            ty: Type::cast_check(Type::from_pk()).unwrap(),
            ext: ExtData::cast_check(ExtData::from_pk()).unwrap(),
        };
        string_rtt(pk_ms, "[B/onduesm]c:[K/onduesm]pk(DummyKey)", "c:pk()");

        let pkh_ms: Miniscript<DummyKey> = Miniscript {
            node: Terminal::Check(Arc::new(Miniscript {
                node: Terminal::PkH(DummyKeyHash),
                ty: Type::from_pk_h(),
                ext: types::extra_props::ExtData::from_pk_h(),
            })),
            ty: Type::cast_check(Type::from_pk_h()).unwrap(),
            ext: ExtData::cast_check(ExtData::from_pk_h()).unwrap(),
        };
        string_rtt(
            pkh_ms,
            "[B/nduesm]c:[K/nduesm]pk_h(DummyKeyHash)",
            "c:pk_h()",
        );

        let pk_ms: Miniscript<bitcoin::PublicKey> = Miniscript {
            node: Terminal::Check(Arc::new(Miniscript {
                node: Terminal::Pk(pk),
                ty: Type::from_pk(),
                ext: types::extra_props::ExtData::from_pk(),
            })),
            ty: Type::cast_check(Type::from_pk()).unwrap(),
            ext: ExtData::cast_check(ExtData::from_pk()).unwrap(),
        };

        script_rtt(
            pk_ms,
            "21020202020202020202020202020202020202020202020202020202020\
             202020202ac",
        );

        let pkh_ms: Miniscript<bitcoin::PublicKey> = Miniscript {
            node: Terminal::Check(Arc::new(Miniscript {
                node: Terminal::PkH(hash),
                ty: Type::from_pk_h(),
                ext: types::extra_props::ExtData::from_pk_h(),
            })),
            ty: Type::cast_check(Type::from_pk_h()).unwrap(),
            ext: ExtData::cast_check(ExtData::from_pk_h()).unwrap(),
        };

        script_rtt(pkh_ms, "76a914111111111111111111111111111111111111111188ac");
    }

    #[test]
    fn serialize() {
        let keys = pubkeys(5);
        let dummy_hash = hash160::Hash::from_inner([0; 20]);

        roundtrip(
            &ms_str!("c:pk_h({})", dummy_hash),
            "\
             Script(OP_DUP OP_HASH160 OP_PUSHBYTES_20 \
             0000000000000000000000000000000000000000 \
             OP_EQUALVERIFY OP_CHECKSIG)\
             ",
        );

        roundtrip(
            &ms_str!("c:pk({})", keys[0]),
            "Script(OP_PUSHBYTES_33 028c28a97bf8298bc0d23d8c749452a32e694b65e30a9472a3954ab30fe5324caa OP_CHECKSIG)"
        );
        roundtrip(
            &ms_str!("thresh_m(3,{},{},{},{},{})", keys[0], keys[1], keys[2], keys[3], keys[4]),
            "Script(OP_PUSHNUM_3 OP_PUSHBYTES_33 028c28a97bf8298bc0d23d8c749452a32e694b65e30a9472a3954ab30fe5324caa OP_PUSHBYTES_33 03ab1ac1872a38a2f196bed5a6047f0da2c8130fe8de49fc4d5dfb201f7611d8e2 OP_PUSHBYTES_33 039729247032c0dfcf45b4841fcd72f6e9a2422631fc3466cf863e87154754dd40 OP_PUSHBYTES_33 032564fe9b5beef82d3703a607253f31ef8ea1b365772df434226aee642651b3fa OP_PUSHBYTES_33 0289637f97580a796e050791ad5a2f27af1803645d95df021a3c2d82eb8c2ca7ff OP_PUSHNUM_5 OP_CHECKMULTISIG)"
        );

        // Liquid policy
        roundtrip(
            &ms_str!("or_d(thresh_m(2,{},{}),and_v(v:thresh_m(2,{},{}),older(10000)))",
                      keys[0].to_string(),
                      keys[1].to_string(),
                      keys[3].to_string(),
                      keys[4].to_string()),
            "Script(OP_PUSHNUM_2 OP_PUSHBYTES_33 028c28a97bf8298bc0d23d8c749452a32e694b65e30a9472a3954ab30fe5324caa \
                                  OP_PUSHBYTES_33 03ab1ac1872a38a2f196bed5a6047f0da2c8130fe8de49fc4d5dfb201f7611d8e2 \
                                  OP_PUSHNUM_2 OP_CHECKMULTISIG \
                     OP_IFDUP OP_NOTIF \
                         OP_PUSHNUM_2 OP_PUSHBYTES_33 032564fe9b5beef82d3703a607253f31ef8ea1b365772df434226aee642651b3fa \
                                      OP_PUSHBYTES_33 0289637f97580a796e050791ad5a2f27af1803645d95df021a3c2d82eb8c2ca7ff \
                                      OP_PUSHNUM_2 OP_CHECKMULTISIGVERIFY \
                         OP_PUSHBYTES_2 1027 OP_CSV \
                     OP_ENDIF)"
        );

        let miniscript: Miniscript<bitcoin::PublicKey> = ms_str!(
            "or_d(thresh_m(3,{},{},{}),and_v(v:thresh_m(2,{},{}),older(10000)))",
            keys[0].to_string(),
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
        );

        let mut abs = miniscript.lift();
        assert_eq!(abs.n_keys(), 5);
        assert_eq!(abs.minimum_n_keys(), 2);
        abs = abs.at_age(10000);
        assert_eq!(abs.n_keys(), 5);
        assert_eq!(abs.minimum_n_keys(), 2);
        abs = abs.at_age(9999);
        assert_eq!(abs.n_keys(), 3);
        assert_eq!(abs.minimum_n_keys(), 3);
        abs = abs.at_age(0);
        assert_eq!(abs.n_keys(), 3);
        assert_eq!(abs.minimum_n_keys(), 3);

        roundtrip(&ms_str!("older(921)"), "Script(OP_PUSHBYTES_2 9903 OP_CSV)");

        roundtrip(
            &ms_str!("sha256({})",sha256::Hash::hash(&[])),
            "Script(OP_SIZE OP_PUSHBYTES_1 20 OP_EQUALVERIFY OP_SHA256 OP_PUSHBYTES_32 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 OP_EQUAL)"
        );

        roundtrip(
            &ms_str!(
                "thresh_m(3,{},{},{},{},{})",
                keys[0],
                keys[1],
                keys[2],
                keys[3],
                keys[4]
            ),
            "Script(OP_PUSHNUM_3 \
             OP_PUSHBYTES_33 028c28a97bf8298bc0d23d8c749452a32e694b65e30a9472a3954ab30fe5324caa \
             OP_PUSHBYTES_33 03ab1ac1872a38a2f196bed5a6047f0da2c8130fe8de49fc4d5dfb201f7611d8e2 \
             OP_PUSHBYTES_33 039729247032c0dfcf45b4841fcd72f6e9a2422631fc3466cf863e87154754dd40 \
             OP_PUSHBYTES_33 032564fe9b5beef82d3703a607253f31ef8ea1b365772df434226aee642651b3fa \
             OP_PUSHBYTES_33 0289637f97580a796e050791ad5a2f27af1803645d95df021a3c2d82eb8c2ca7ff \
             OP_PUSHNUM_5 OP_CHECKMULTISIG)",
        );
    }

    #[test]
    fn deserialize() {
        // Most of these came from fuzzing, hence the increasing lengths
        assert!(Miniscript::parse(&hex_script("")).is_err()); // empty
        assert!(Miniscript::parse(&hex_script("00")).is_ok()); // FALSE
        assert!(Miniscript::parse(&hex_script("51")).is_ok()); // TRUE
        assert!(Miniscript::parse(&hex_script("69")).is_err()); // VERIFY
        assert!(Miniscript::parse(&hex_script("0000")).is_err()); //and_v(FALSE,FALSE)
        assert!(Miniscript::parse(&hex_script("1001")).is_err()); // incomplete push
        assert!(Miniscript::parse(&hex_script("03990300b2")).is_err()); // non-minimal #
        assert!(Miniscript::parse(&hex_script("8559b2")).is_err()); // leading bytes
        assert!(Miniscript::parse(&hex_script("4c0169b2")).is_err()); // non-minimal push
        assert!(Miniscript::parse(&hex_script("0000af0000ae85")).is_err()); // OR not BOOLOR

        // misc fuzzer problems
        assert!(Miniscript::parse(&hex_script("0000000000af")).is_err());
        assert!(Miniscript::parse(&hex_script("04009a2970af00")).is_err()); // giant CMS key num
        assert!(Miniscript::parse(&hex_script(
            "2102ffffffffffffffefefefefefefefefefefef394c0fe5b711179e124008584753ac6900"
        ))
        .is_err());
    }
}
