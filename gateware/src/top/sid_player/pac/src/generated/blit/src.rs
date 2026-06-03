#[doc = "Register `src` reader"]
pub type R = crate::R<SRC_SPEC>;
#[doc = "Register `src` writer"]
pub type W = crate::W<SRC_SPEC>;
#[doc = "Field `src_x` writer - src_x field"]
pub type SRC_X_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `src_y` writer - src_y field"]
pub type SRC_Y_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `width` writer - width field"]
pub type WIDTH_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `height` writer - height field"]
pub type HEIGHT_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
impl W {
    #[doc = "Bits 0:7 - src_x field"]
    #[inline(always)]
    pub fn src_x(&mut self) -> SRC_X_W<'_, SRC_SPEC> {
        SRC_X_W::new(self, 0)
    }
    #[doc = "Bits 8:15 - src_y field"]
    #[inline(always)]
    pub fn src_y(&mut self) -> SRC_Y_W<'_, SRC_SPEC> {
        SRC_Y_W::new(self, 8)
    }
    #[doc = "Bits 16:23 - width field"]
    #[inline(always)]
    pub fn width(&mut self) -> WIDTH_W<'_, SRC_SPEC> {
        WIDTH_W::new(self, 16)
    }
    #[doc = "Bits 24:31 - height field"]
    #[inline(always)]
    pub fn height(&mut self) -> HEIGHT_W<'_, SRC_SPEC> {
        HEIGHT_W::new(self, 24)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`src::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`src::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct SRC_SPEC;
impl crate::RegisterSpec for SRC_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`src::R`](R) reader structure"]
impl crate::Readable for SRC_SPEC {}
#[doc = "`write(|w| ..)` method takes [`src::W`](W) writer structure"]
impl crate::Writable for SRC_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets src to value 0"]
impl crate::Resettable for SRC_SPEC {}
